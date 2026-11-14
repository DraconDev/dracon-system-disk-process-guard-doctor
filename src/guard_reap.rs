//! The guard's reap and agent-session lifecycle passes: the glue between
//! the guard policy and the scanners in `reap.rs`, `forgotten.rs` and
//! `session_guard.rs`, plus the read-only `guard reap-preview` command.
//!
//! Split out of main.rs 2026-11-14 (audit task list, P3). Moved verbatim;
//! items are `pub(crate)` and re-exported from main.rs.

use super::*;

pub(crate) fn reap_policy_from_guard(guard: &GuardPolicy) -> ReapPolicy {
    ReapPolicy {
        min_idle_hours: guard.reap_report_min_idle_hours,
        max_cpu_seconds: guard.reap_report_max_cpu_seconds,
        signatures: sorted_signatures(&guard.reap_report_signatures),
        exempt_names: sorted_signatures(&guard.process_exempt_names),
        forgotten_footprint_mb: guard.forgotten_orphan_footprint_mb,
        forgotten_tree_footprint_mb: guard.forgotten_orphan_tree_footprint_mb,
    }
}

/// Build the agent-session lifecycle policy from the guard policy.
pub(crate) fn session_policy_from_guard(guard: &GuardPolicy) -> SessionPolicy {
    SessionPolicy {
        session_names: sorted_signatures(&guard.agent_session_names),
        idle_window_secs: guard.session_idle_window_secs,
        max_active_sessions: guard.max_active_sessions,
        registry_dir: expand_tilde(&guard.session_registry_dir),
    }
}

/// The per-process facts the session tracker needs, read from /proc.
///
/// The tracker's CPU baseline is the CUMULATIVE cpu time, so it must come
/// from `/proc/<pid>/stat` — `ps`'s instantaneous percentage cannot prove
/// "no CPU accrual across the window", which is the whole point of the
/// idle-retirement rule.
pub(crate) fn session_process_facts(policy: &SessionPolicy, proc_root: &Path) -> Vec<(i32, String, u64, u64)> {
    if policy.session_names.is_empty() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<i32>() else {
            continue;
        };
        let pid_dir = entry.path();
        let Ok(comm) = std::fs::read_to_string(pid_dir.join("comm")) else {
            continue;
        };
        let comm = comm.trim().to_string();
        if !is_session_name(&comm, &policy.session_names) {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(pid_dir.join("stat")) else {
            continue;
        };
        let Some(fields) = crate::reap::parse_stat(&raw) else {
            continue;
        };
        out.push((pid, comm, fields.cpu_ticks, fields.starttime));
    }
    out
}

/// Observe the agent-session fleet and recompute the exemption set.
///
/// This is the fleet-aware half of the exemption: a fleet of agent
/// sessions no longer inherits a blanket pass over the memory-pressure
/// mitigation. When the fleet is over its bound, only the demonstrably
/// active sessions keep the exemption.
pub(crate) fn refresh_session_fleet(guard: &GuardPolicy, state: &mut GuardRuntimeState) {
    let policy = session_policy_from_guard(guard);
    if policy.session_names.is_empty() {
        state.session_tracker = SessionTracker::default();
        state.session_exempt_pids = None;
        state.session_observations.clear();
        state.session_states.clear();
        return;
    }
    let proc_root = Path::new("/proc");
    let now = std::time::SystemTime::now();
    let now_instant = Instant::now();
    let facts = session_process_facts(&policy, proc_root);
    let observations = state
        .session_tracker
        .observe(&policy, proc_root, &facts, now, now_instant);
    let states = state.session_tracker.states(&policy, now, now_instant);

    // Forget the sessions that are gone, so the tracker cannot itself
    // become the unbounded thing it is here to prevent.
    let live: HashSet<i32> = facts.iter().map(|(pid, _, _, _)| *pid).collect();
    state.session_tracker.retain_live(&live);

    let exempt_names = sorted_signatures(&guard.process_exempt_names);
    // The Option carries the distinction: `None` = lifecycle not
    // configured, `Some(empty)` = configured and nobody holds the pass.
    state.session_exempt_pids = exemption_pids(&policy, &exempt_names, &observations, &states);

    // The fleet summary is part of the REPORT, not just internal state:
    // the fleet-aware exemption is a policy, and the operator has to be
    // able to see which sessions hold the blanket pass and why.
    let mut active = 0usize;
    let mut idle = 0usize;
    let mut unknown = 0usize;
    for observation in &observations {
        match states.get(&observation.pid).copied() {
            Some(SessionState::Active) => active += 1,
            Some(SessionState::Idle) => idle += 1,
            _ => unknown += 1,
        }
    }
    state.session_fleet = SessionFleetReport {
        names: policy.session_names.clone(),
        total: observations.len(),
        active,
        idle,
        unknown,
        max_active_sessions: policy.max_active_sessions,
        over_bound: observations.len() > policy.max_active_sessions,
        exempt: state
            .session_exempt_pids
            .as_ref()
            .map_or(0, |pids| pids.len()),
    };
    state.session_observations = observations;
    state.session_states = states;
}

/// Retire the agent sessions that are demonstrably idle.
///
/// This is the ending half of the session lifecycle. The candidate set
/// comes from the tracker (`SessionState::Idle`), which already encodes
/// BOTH conditions of the rule: no CPU accrual across the idle window and
/// no ledger/transcript write within it.
///
/// Every candidate is re-verified immediately before signalling, so a
/// session that woke up between the scan and the signal is left alone.
/// The kill itself is SIGTERM with a SIGKILL escalation, the same
/// discipline the dev-server reaper uses.
pub(crate) async fn retire_idle_sessions(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
) -> Vec<RetiredSession> {
    let policy = session_policy_from_guard(guard);
    if policy.session_names.is_empty() {
        return Vec::new();
    }
    let now = std::time::SystemTime::now();
    // The fleet-over-bound trigger: an individual session keeps its
    // protection; only an unbounded fleet gives it up.
    let fleet_over_bound = state.session_fleet.over_bound;
    let candidates = retirement_candidates(
        &state.session_observations,
        &state.session_states,
        policy.idle_window_secs,
        now,
        fleet_over_bound,
    );
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let idle_window = policy.idle_window_secs;
        let verified = verify_retirement(Path::new("/proc"), candidate, idle_window, now);
        if !verified {
            eprintln!(
                "🛡️ session-retire: pid {} ({}) no longer verifies -- skipped, nothing signalled",
                candidate.pid, candidate.comm,
            );
            out.push(RetiredSession {
                pid: candidate.pid,
                comm: candidate.comm.clone(),
                cpu_idle_secs: policy.idle_window_secs,
                activity_idle_secs: candidate.idle_secs,
                verified: false,
                outcome: None,
                dry_run: guard.retire_idle_agent_sessions_dry_run,
            });
            continue;
        }
        if guard.retire_idle_agent_sessions_dry_run {
            eprintln!(
                "🛡️ session-retire (dry run): pid {} ({}) idle {}s (no CPU accrual, no ledger/transcript write) -> WOULD retire; nothing signalled",
                candidate.pid, candidate.comm, candidate.idle_secs,
            );
            out.push(RetiredSession {
                pid: candidate.pid,
                comm: candidate.comm.clone(),
                cpu_idle_secs: policy.idle_window_secs,
                activity_idle_secs: candidate.idle_secs,
                verified: true,
                outcome: None,
                dry_run: true,
            });
            continue;
        }
        let outcome = terminate_process(candidate.pid);
        eprintln!(
            "🛡️ session-retire: pid {} ({}) idle {}s (no CPU accrual, no ledger/transcript write) -> {:?}",
            candidate.pid, candidate.comm, candidate.idle_secs, outcome,
        );
        out.push(RetiredSession {
            pid: candidate.pid,
            comm: candidate.comm.clone(),
            cpu_idle_secs: policy.idle_window_secs,
            activity_idle_secs: candidate.idle_secs,
            verified: true,
            outcome: Some(outcome),
            dry_run: false,
        });
    }
    out
}

/// Scan for forgotten heavy orphan TREES: heavy, idle, orphaned, and
/// provably a test job.
///
/// ADDED 2026-10-22 as a per-process scan; CHANGED 2026-10-23 to weigh a
/// whole tree, because the leak that cost 8.6 GB was 792 processes of
/// ~3 MiB each and no single one of them ever crossed a per-process
/// floor. Never signals anything: this is the report half. The ending
/// half is `reap_forgotten_orphan_trees`, and it runs only under memory
/// pressure.
pub(crate) fn forgotten_scan(guard: &GuardPolicy) -> ForgottenTreeScan {
    let proc_root = Path::new("/proc");
    let Some(boot_time) = read_boot_time(proc_root) else {
        return ForgottenTreeScan::default();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(boot_time);
    let policy = reap_policy_from_guard(guard);
    scan_forgotten_orphan_trees(
        proc_root,
        &policy,
        boot_time,
        now,
        PROC_TICKS_PER_SEC,
        guard.forgotten_orphan_footprint_mb,
        guard.forgotten_orphan_tree_footprint_mb,
    )
}

/// Report abandoned dev/test processes. Never signals, kills, or stops
/// anything -- see `reap.rs`. Returns empty on a host where procfs cannot
/// be read, so a missing clock degrades the report instead of failing the
/// pass.
pub(crate) fn reap_candidates(guard: &GuardPolicy) -> Vec<ReapCandidate> {
    let proc_root = Path::new("/proc");
    let Some(boot_time) = read_boot_time(proc_root) else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(boot_time);
    scan_reap_candidates(
        proc_root,
        &reap_policy_from_guard(guard),
        boot_time,
        now,
        PROC_TICKS_PER_SEC,
    )
}

/// Pressure-orphan candidates against the live /proc (no signal).
pub(crate) fn pressure_orphan_preview(guard: &GuardPolicy) -> Vec<ReapCandidate> {
    let proc_root = Path::new("/proc");
    let Some(boot_time) = read_boot_time(proc_root) else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(boot_time);
    scan_pressure_orphans(
        proc_root,
        &reap_policy_from_guard(guard),
        boot_time,
        now,
        PROC_TICKS_PER_SEC,
    )
}

#[derive(Serialize)]
pub(crate) struct ReapPreviewPass<'a> {
    pub(crate) pass: &'a str,
    pub(crate) knob: &'a str,
    pub(crate) enabled: bool,
    pub(crate) gate: &'a str,
    pub(crate) candidates: Vec<String>,
}

pub(crate) fn reap_preview_row(c: &ReapCandidate) -> String {
    let args: String = c.args.chars().take(140).collect();
    format!(
        "pid {} {} [{}] age {}h cpu {}s rss {}MiB :: {}",
        c.pid, c.comm, c.signature, c.idle_hours, c.cpu_seconds, c.rss_mb, args
    )
}

pub(crate) fn cmd_guard_reap_preview(guard: &GuardPolicy, json: bool) -> Result<()> {
    let stale: Vec<String> = reap_candidates(guard)
        .iter()
        .map(reap_preview_row)
        .collect();
    let pressure: Vec<String> = pressure_orphan_preview(guard)
        .iter()
        .map(reap_preview_row)
        .collect();
    let forgotten: Vec<String> = forgotten_scan(guard)
        .candidates
        .iter()
        .map(|t| {
            format!(
                "tree root {} {} — {} processes {}MiB age {}s — {}",
                t.root_pid,
                t.root_comm,
                t.member_count,
                t.aggregate_footprint_mb,
                t.age_secs,
                t.why
            )
        })
        .collect();
    let passes = vec![
        ReapPreviewPass {
            pass: "stale dev servers",
            knob: "reap_stale_dev_servers",
            enabled: guard.reap_stale_dev_servers,
            gate: "every pass; idle, sleeping, no tty, age floor, signature, not a service",
            candidates: stale,
        },
        ReapPreviewPass {
            pass: "pressure orphans",
            knob: "reap_orphans_on_pressure",
            enabled: guard.reap_orphans_on_pressure,
            gate: "only under warn/critical memory pressure; owner dead, not a service, launching session gone",
            candidates: pressure,
        },
        ReapPreviewPass {
            pass: "forgotten browser trees",
            knob: "no knob, always on",
            enabled: true,
            gate: "only under memory pressure; orphaned headless test browser, undriven, heavy",
            candidates: forgotten,
        },
    ];
    if json {
        println!("{}", serde_json::to_string_pretty(&passes)?);
        return Ok(());
    }
    println!("Reap preview — read-only, nothing is signalled.");
    for p in &passes {
        let state = if p.enabled {
            "ENABLED"
        } else {
            "disabled (report only)"
        };
        println!();
        if p.knob.contains(' ') {
            println!("■ {} [{}] — {}", p.pass, p.knob, p.gate);
        } else {
            println!("■ {} [{} = {}] — {}", p.pass, p.knob, state, p.gate);
        }
        if p.candidates.is_empty() {
            println!("  (none)");
        }
        for c in &p.candidates {
            println!("  • {c}");
        }
    }
    println!();
    println!(
        "■ idle agent sessions [retire_idle_agent_sessions = {}, dry_run = {}] — needs {}s of observation by the running daemon; see its journal for 'session-retire' lines",
        guard.retire_idle_agent_sessions,
        guard.retire_idle_agent_sessions_dry_run,
        guard.session_idle_window_secs
    );
    Ok(())
}
