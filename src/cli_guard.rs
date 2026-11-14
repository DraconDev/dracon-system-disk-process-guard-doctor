//! The `dracon-system guard …` subcommands: once, daemon, prune, clean, and dispatch.
//!
//! Split out of main.rs 2026-11-14 (audit task list, P3). Moved verbatim;
//! items are `pub(crate)` and re-exported from main.rs.

use super::*;

pub(crate) async fn cmd_guard_once(guard: &GuardPolicy, json: bool) -> Result<()> {
    // One process owns hydrate/mutate/persist for the entire lifetime, just
    // like the daemon. A diagnostic may not drain the running guard's state.
    let _owner = acquire_daemon_lock("dracon-system-guard").with_context(|| {
        "guard already owns process mitigation state; refusing concurrent guard once"
    })?;
    use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};

    // Only the daemon persists: a one-shot run restores everything before it
    // exits, and nothing else should write the operator's state dir.
    let mut runtime = GuardRuntimeState {
        persist_mitigations: true,
        ..GuardRuntimeState::default()
    };
    // AUDIT 2026-10-01: a previous run may have died (panic, OOM kill) with
    // mitigations applied. Adopt them into the live state and drain them through
    // the EXISTING restore path before the first pass, so no process is left
    // reniced/biased/capped forever. Identity is re-verified on adoption, so a
    // recycled PID is never touched.
    cpu_limit::recover().await;
    hydrate_mitigations_from_disk(&mut runtime);
    if !runtime.memory_reniced_pids.is_empty()
        || !runtime.oom_biased_pids.is_empty()
        || !runtime.capped_pids.is_empty()
    {
        restore_runtime_adjustments(&mut runtime).await;
    }
    let report_result = run_guard_once(guard, &mut runtime).await;
    // A one-shot invocation has no daemon runtime to carry these entries into
    // a later retry. Restore every limiter before handling either a report or
    // an error, including the JSON early-return path.
    let adjustments_restored = restore_runtime_adjustments(&mut runtime).await;
    if !adjustments_restored {
        eprintln!("⚠ guard once: some process adjustments could not be restored");
    }
    let report = report_result?;
    if !adjustments_restored {
        anyhow::bail!("guard once could not restore every process adjustment");
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    // ── Disk health ──
    let disk = disk_details_for(&guard.disk_mount_path).await.ok();
    let state_label = report.disk_state.as_str();
    let state_icon = match state_label {
        "ok" => "✅",
        "warn" => "⚠️",
        "action" => "🟠",
        "critical" => "🔴",
        _ => "",
    };

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("STATUS"),
            Cell::new("CHECK"),
            Cell::new("VALUE"),
        ]);

    table.add_row(vec![
        Cell::new(if report.enabled { "✅" } else { "❌" }),
        Cell::new("Guard"),
        Cell::new(if report.enabled {
            "enabled"
        } else {
            "disabled"
        }),
    ]);

    if let Some(ref d) = disk {
        table.add_row(vec![
            Cell::new(state_icon),
            Cell::new("Disk Usage"),
            Cell::new(format!(
                "{}% ({}) — {} / {}",
                d.use_percent,
                state_label,
                human_bytes(d.used_bytes),
                human_bytes(d.total_bytes)
            )),
        ]);
        table.add_row(vec![
            Cell::new(""),
            Cell::new("Disk Free"),
            Cell::new(format!("{} on {}", human_bytes(d.avail_bytes), d.mount,)),
        ]);
    } else {
        table.add_row(vec![
            Cell::new(state_icon),
            Cell::new("Disk Usage"),
            Cell::new(format!("{}% ({})", report.disk_use_percent, state_label)),
        ]);
    }

    for m in &report.extra_mounts {
        let icon = match m.state.as_str() {
            "ok" => "✅",
            "warn" => "⚠️",
            "action" => "🟠",
            "critical" => "🔴",
            _ => "",
        };
        table.add_row(vec![
            Cell::new(icon),
            Cell::new(format!("Disk {}", m.mount)),
            Cell::new(format!("{}% ({})", m.use_percent, m.state)),
        ]);
    }

    if report.relocated_count > 0 {
        table.add_row(vec![
            Cell::new("📦"),
            Cell::new("Auto-relocated"),
            Cell::new(format!(
                "{} dir(s), {}",
                report.relocated_count,
                human_bytes(report.relocated_bytes)
            )),
        ]);
    }
    for (i, c) in report.relocate_candidates.iter().enumerate() {
        if i >= 5 {
            table.add_row(vec![
                Cell::new(""),
                Cell::new("Cold candidates"),
                Cell::new(format!(
                    "…and {} more",
                    report.relocate_candidates.len() - 5
                )),
            ]);
            break;
        }
        table.add_row(vec![
            Cell::new("📦"),
            Cell::new(if i == 0 { "Cold candidates" } else { "" }),
            Cell::new(format!(
                "{} ({} in {} files, {}d idle)",
                c.path,
                human_bytes(c.bytes),
                c.files,
                c.age_days
            )),
        ]);
    }

    table.add_row(vec![
        Cell::new(if report.sync_frozen { "⏸️" } else { "" }),
        Cell::new("Sync Frozen"),
        Cell::new(if report.sync_frozen { "yes" } else { "no" }),
    ]);

    table.add_row(vec![
        Cell::new(""),
        Cell::new("Thresholds"),
        Cell::new(format!(
            "warn={}% action={}% critical={}%",
            guard.disk_warn_percent, guard.disk_action_percent, guard.disk_critical_percent
        )),
    ]);

    table.add_row(vec![
        Cell::new(""),
        Cell::new("Process Monitor"),
        Cell::new(format!(
            "cpu>{}% for >{}s, auto_renice={}",
            guard.process_cpu_percent, guard.process_sustain_secs, guard.auto_renice
        )),
    ]);

    if report.alerts.is_empty() {
        table.add_row(vec![
            Cell::new("✅"),
            Cell::new("Heavy Processes"),
            Cell::new("none"),
        ]);
    } else {
        table.add_row(vec![
            Cell::new("⚠️"),
            Cell::new("Heavy Processes"),
            Cell::new(format!("{} active", report.alerts.len())),
        ]);
    }

    // ADDED 2026-08-10 (v0.112.35): memory/swap pressure row.
    if let Some(ref m) = report.memory {
        let icon = match m.pressure.as_str() {
            "critical" => "🔴",
            "warn" => "⚠️",
            _ => "✅",
        };
        table.add_row(vec![
            Cell::new(icon),
            Cell::new("Memory Pressure"),
            Cell::new(format!(
                "{}: avail {}% swap {}%{} limited: {}",
                if m.observed_pressure != m.pressure {
                    format!("{} (observed {})", m.pressure, m.observed_pressure)
                } else {
                    m.pressure.clone()
                },
                m.mem_available_percent,
                m.swap_used_percent,
                m.psi_full_avg10
                    .map(|v| format!(" PSI-full {:.1}%", v))
                    .unwrap_or_default(),
                if m.limited.is_empty() {
                    "-".to_string()
                } else {
                    m.limited.join(", ")
                }
            )),
        ]);
        // ADDED 2026-11-08: say WHY nothing was (or was) mitigated. The
        // offenders list excludes exempt processes by policy, so without
        // this the row could not distinguish "handled" from "not
        // permitted to touch what is actually consuming the memory".
        table.add_row(vec![
            Cell::new(""),
            Cell::new("Memory Pressure (mitigation)"),
            Cell::new(m.mitigation_note.clone()),
        ]);
    }

    // ADDED 2026-10-27 (wrong-swap check): periodic placement/reference
    // verdict. Unknown (unreadable /proc/swaps or config) renders as
    // "unchecked" — never as a failure.
    table.add_row(vec![
        Cell::new(if report.swap_issues.is_empty() {
            "✅"
        } else {
            "⚠️"
        }),
        Cell::new("Swap Devices"),
        Cell::new(if report.swap_issues.is_empty() {
            "all active swap on expected devices".to_string()
        } else {
            report.swap_issues.join("; ")
        }),
    ]);

    // ADDED 2026-08-10 (v0.112.35): zombie + fill-rate rows.
    table.add_row(vec![
        Cell::new(if report.zombies.is_empty() {
            "✅"
        } else {
            "⚠️"
        }),
        Cell::new("Zombies"),
        Cell::new(if report.zombies.is_empty() {
            "none".to_string()
        } else {
            format!(
                "{} (oldest {}s, e.g. pid={} {})",
                report.zombies.len(),
                report.zombies[0].age_secs,
                report.zombies[0].pid,
                report.zombies[0].comm
            )
        }),
    ]);
    table.add_row(vec![
        Cell::new(""),
        Cell::new("Disk Fill Rate"),
        Cell::new(
            report
                .disk_fill_gbph
                .map(|r| format!("{:.1} GiB/h", r))
                .unwrap_or_else(|| "< 0.1 GiB/h or settling".to_string()),
        ),
    ]);

    // ADDED 2026-10-08 (audit F118): the reaper's audit trail must be
    // visible to a human running `guard once` without `--json`. `reaped` /
    // `pressure_reaped` were serialized to JSON and stderr only, so the
    // operator saw zero evidence that processes were killed. Same class as
    // the relocation/stash visibility fixes: a destructive action with
    // no table row is an action nobody can review.
    if !guard.reap_stale_dev_servers {
        table.add_row(vec![
            Cell::new(""),
            Cell::new("Reap candidates"),
            Cell::new(if report.reap_candidates.is_empty() {
                "none".to_string()
            } else {
                format!(
                    "{} (reap_stale_dev_servers is OFF — report only)",
                    report.reap_candidates.len()
                )
            }),
        ]);
    }
    if !report.reaped.is_empty() {
        table.add_row(vec![
            Cell::new("🪓"),
            Cell::new("Reaped (stale servers)"),
            Cell::new(summarize_reaped(&report.reaped)),
        ]);
    }
    table.add_row(vec![
        Cell::new(""),
        Cell::new("Service memory growth (report only)"),
        Cell::new(report.service_memory_growth.summary()),
    ]);
    if let Some(memory) = &report.memory {
        if !memory.pressure_reaped.is_empty() {
            table.add_row(vec![
                Cell::new("🪓"),
                Cell::new("Reaped (pressure orphans)"),
                Cell::new(summarize_reaped(&memory.pressure_reaped)),
            ]);
        }
        if !memory.forgotten_trees.is_empty() {
            let mut parts: Vec<String> = memory
                .forgotten_trees
                .iter()
                .map(|t| {
                    format!(
                        "pid={} {} proc {}MiB",
                        t.root_pid, t.member_count, t.aggregate_footprint_mb
                    )
                })
                .collect();
            parts.sort();
            table.add_row(vec![
                Cell::new("🧹"),
                Cell::new("Forgotten orphan trees"),
                Cell::new(format!(
                    "{} — {}",
                    memory.forgotten_trees.len(),
                    parts.join(", ")
                )),
            ]);
        }
        if !memory.forgotten_reaped.is_empty() {
            let ended: usize = memory
                .forgotten_reaped
                .iter()
                .map(|r| r.members_ended)
                .sum();
            table.add_row(vec![
                Cell::new("🪓"),
                Cell::new("Reaped (forgotten trees)"),
                Cell::new(format!(
                    "{} tree(s), {ended} process(es) ended",
                    memory.forgotten_reaped.len()
                )),
            ]);
        }
        if !report.session_fleet.names.is_empty() {
            table.add_row(vec![
                Cell::new("🧠"),
                Cell::new("Agent session fleet"),
                Cell::new(format!(
                    "{} session(s): {} active, {} idle, {} unmeasured — bound {}{}; {} keep(s) the blanket exemption",
                    report.session_fleet.total,
                    report.session_fleet.active,
                    report.session_fleet.idle,
                    report.session_fleet.unknown,
                    report.session_fleet.max_active_sessions,
                    if report.session_fleet.over_bound {
                        " (OVER BOUND — exemption narrowed)"
                    } else {
                        ""
                    },
                    report.session_fleet.exempt
                )),
            ]);
        }
        if !memory.vetoed_trees.is_empty() {
            table.add_row(vec![
                Cell::new("🛡️"),
                Cell::new("Heavy trees vetoed"),
                Cell::new(format!(
                    "{} (left alone — a live desktop session)",
                    memory.vetoed_trees.len()
                )),
            ]);
        }
        if !memory.protected_trees.is_empty() {
            let mut parts: Vec<String> = memory
                .protected_trees
                .iter()
                .map(|t| {
                    format!(
                        "pid={} {} proc {}MiB — {}",
                        t.root_pid, t.member_count, t.aggregate_footprint_mb, t.reason
                    )
                })
                .collect();
            parts.sort();
            table.add_row(vec![
                Cell::new("🛡️"),
                Cell::new("Heavy trees left alone"),
                Cell::new(format!(
                    "{} — {}",
                    memory.protected_trees.len(),
                    parts.join(", ")
                )),
            ]);
        }
    }

    println!("{table}");

    // ── Process detail table ──
    if !report.alerts.is_empty() {
        let mut ptable = Table::new();
        ptable
            .load_preset(UTF8_FULL_CONDENSED)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("PID"),
                Cell::new("CPU"),
                Cell::new("RSS"),
                Cell::new("SUSTAINED"),
                Cell::new("ACTION"),
                Cell::new("NICE"),
                Cell::new("COMMAND"),
            ]);
        for a in &report.alerts {
            ptable.add_row(vec![
                Cell::new(a.pid),
                Cell::new(format!("{:.1}%", a.cpu_percent)),
                Cell::new(format!("{}MiB", a.rss_mb)),
                Cell::new(format!("{}s", a.sustained_secs)),
                Cell::new(&a.action),
                Cell::new(a.nice_value),
                Cell::new(if a.args.is_empty() {
                    a.command.clone()
                } else {
                    format!("{} {}", a.command, a.args)
                }),
            ]);
        }
        println!();
        println!("Heavy processes:");
        println!("{ptable}");
    }

    Ok(())
}

pub(crate) fn rotate_guard_log_for_policy(guard: &GuardPolicy, startup: bool) {
    if let Some(log_path) = resolve_guard_log_path(&guard.guard_log_file) {
        let max_bytes = guard.guard_log_max_mb.saturating_mul(1024 * 1024);
        rotate_guard_log_if_oversized(&log_path, max_bytes, startup);
    }
}

#[cfg(test)]
pub(crate) fn rotate_guard_log_for_policy_with_home(
    guard: &GuardPolicy,
    startup: bool,
    home: Option<&Path>,
) {
    if let Some(log_path) = resolve_guard_log_path_with_home(&guard.guard_log_file, home) {
        let max_bytes = guard.guard_log_max_mb.saturating_mul(1024 * 1024);
        rotate_guard_log_if_oversized(&log_path, max_bytes, startup);
    }
}

/// What a SIGHUP reload attempt did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReloadOutcome {
    /// The new policy is live. Runtime state was cleanly reset, so nothing
    /// from the previous policy is still in force.
    Applied,
    /// The new policy was NOT adopted. The previous policy stays live so its
    /// mitigations remain armed and the leftover process adjustments are
    /// retried on the next pass instead of being stranded.
    Deferred,
}

/// Adopt a freshly loaded policy, or refuse to.
///
/// Bounded by design: this touches the policy and the in-memory runtime
/// state, and nothing else. It performs no I/O, spawns nothing, and does not
/// read the policy file, so the whole reload decision is testable in-process.
///
/// ADDED 2026-09-29 (DECIDE #2 follow-up): the reload previously swapped the
/// policy UNCONDITIONALLY, before checking whether the pre-swap restore of
/// process adjustments had fully succeeded. On a partial restore the leftover
/// `runtime` entries describe adjustments made under the OLD policy, and
/// pairing them with the new one can strand them permanently: if the new
/// policy disables the mitigation that produced them — `auto_renice = false`
/// or `auto_renice_on_memory = false` — nothing will ever retry the restore,
/// and the process stays reniced for the lifetime of the daemon. Keeping the
/// old policy is the safe failure mode: behaviour is exactly what it was
/// before the SIGHUP, and the still-armed mitigation retries next pass.
///
/// `enabled = false` is always applied even after a partial restore: stopping
/// is the safe direction, and the retained state is retried by the shutdown
/// restore in the daemon loop before it returns.
pub(crate) fn apply_policy_reload(
    guard: &mut GuardPolicy,
    runtime: &mut GuardRuntimeState,
    new_policy: GuardPolicy,
    adjustments_restored: bool,
) -> ReloadOutcome {
    let disabling = !new_policy.enabled;
    if (!adjustments_restored || runtime.mitigation_ledger_unavailable) && !disabling {
        return ReloadOutcome::Deferred;
    }
    *guard = new_policy;
    normalize_guard_policy(guard);
    if adjustments_restored && !runtime.mitigation_ledger_unavailable {
        let persist_mitigations = runtime.persist_mitigations;
        *runtime = GuardRuntimeState {
            persist_mitigations,
            ..GuardRuntimeState::default()
        };
    }
    ReloadOutcome::Applied
}

pub(crate) async fn cmd_guard_daemon(guard: &mut GuardPolicy) -> Result<()> {
    if !guard.enabled {
        println!("guard disabled in policy");
        return Ok(());
    }
    let _lock = acquire_daemon_lock("dracon-system-guard")
        .with_context(|| "failed to acquire guard daemon lock")?;

    // ── Startup cleanup: rotate the configured guard log if oversized ──
    // Use the same resolver as event logging; a blank path disables both
    // logging and rotation rather than silently targeting /tmp.
    rotate_guard_log_for_policy(guard, true);

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_sigterm = shutdown.clone();
    let shutdown_sigint = shutdown.clone();
    let reload = Arc::new(AtomicBool::new(false));
    let reload_sighup = reload.clone();
    let reload_sighup_handler = reload.clone();

    tokio::spawn(async move {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
            veprintln!(1, "system: received SIGTERM, shutting down gracefully...");
            shutdown_sigterm.store(true, Ordering::SeqCst);
        } else {
            eprintln!("system: failed to set up SIGTERM handler");
        }
    });

    tokio::spawn(async move {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        {
            sig.recv().await;
            veprintln!(1, "system: received SIGINT, shutting down gracefully...");
            shutdown_sigint.store(true, Ordering::SeqCst);
        } else {
            eprintln!("system: failed to set up SIGINT handler");
        }
    });

    tokio::spawn(async move {
        if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        {
            while sig.recv().await.is_some() {
                veprintln!(1, "system: received SIGHUP, reloading policy...");
                reload_sighup_handler.store(true, Ordering::SeqCst);
            }
        } else {
            eprintln!("system: failed to set up SIGHUP handler");
        }
    });

    veprintln!(
        1,
        "guard daemon started (interval={}s)",
        guard.interval_secs
    );
    let mut interval = guard.interval_secs;
    let mut runtime = GuardRuntimeState {
        persist_mitigations: true,
        ..GuardRuntimeState::default()
    };
    // Recovery belongs to the daemon, not merely the one-shot CLI path.
    cpu_limit::recover().await;
    hydrate_mitigations_from_disk(&mut runtime);
    restore_runtime_adjustments(&mut runtime).await;
    persist_mitigations(&mut runtime);
    let mut binary_replaced_warned = false;
    // ADDED 2026-11-14: is our own watchdog armed? Checked every 10 min;
    // warned once per outage. The guard cannot restart a stopped timer
    // without overriding a deliberate stop, but it can make the stop seen.
    let mut watchdog_checked_at: Option<Instant> = None;
    let mut watchdog_off_warned = false;
    while !shutdown.load(Ordering::SeqCst) {
        if watchdog_checked_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(600)) {
            watchdog_checked_at = Some(Instant::now());
            let hold = dirs::home_dir()
                .map(|h| h.join(".dracon/dracon-system.maintenance-hold"))
                .is_some_and(|p| p.exists());
            let armed = is_user_service_active(GUARD_WATCHDOG_TIMER).await;
            if armed || hold {
                watchdog_off_warned = false;
            } else if !watchdog_off_warned && resolve_bin_for_readonly_probe("systemctl").is_ok() {
                watchdog_off_warned = true;
                let body = format!(
                    "{GUARD_WATCHDOG_TIMER} is not active and no maintenance hold is set — if the guard stops now nothing restarts it (systemctl --user enable --now {GUARD_WATCHDOG_TIMER})"
                );
                eprintln!("⚠️ {body}");
                emit_event(&DraconEvent::new(
                    "system",
                    EventSeverity::Warn,
                    "guard/watchdog-off",
                    body.clone(),
                ));
                send_notification(guard, "Dracon guard: watchdog off", &body).await;
            }
        }
        // ADDED 2026-11-14: notice our OWN binary being replaced. A restart
        // that raced an install kept the guard on the old binary, healthy-
        // looking, with fixes "deployed" but not running. Warn once; the
        // operator (or scripts/deploy-local.sh) restarts.
        if !binary_replaced_warned {
            if let Ok(link) = std::fs::read_link("/proc/self/exe") {
                let link = link.to_string_lossy();
                if exe_link_is_replaced(&link) {
                    binary_replaced_warned = true;
                    let body = format!(
                        "the guard is running a replaced binary ({link}); restart dracon-system-guard.service to run the installed one"
                    );
                    eprintln!("⚠️ {body}");
                    emit_event(&DraconEvent::new(
                        "system",
                        EventSeverity::Warn,
                        "guard/binary-replaced",
                        body.clone(),
                    ));
                    send_notification(guard, "Dracon guard: restart needed", &body).await;
                }
            }
        }
        if runtime.mitigation_ledger_unavailable {
            hydrate_mitigations_from_disk(&mut runtime);
        }
        if reload_sighup.load(Ordering::SeqCst) {
            reload_sighup.store(false, Ordering::SeqCst);
            let result = load_system_policy();
            match result {
                Ok((policy_path, new_policy)) => {
                    if policy_path.is_none() {
                        eprintln!(
                            "system: SIGHUP reload warning: no policy file found, using defaults"
                        );
                        emit_event(&DraconEvent::new(
                            "system",
                            EventSeverity::Warn,
                            "guard/policy-reload",
                            "SIGHUP reload: no policy file found, using defaults".to_string(),
                        ));
                    }
                    // Restore every reversible process adjustment before
                    // discarding the old runtime. This includes the v0.112.36
                    // memory-renice, OOM-bias, and CPUQuota maps as well as
                    // the legacy heavy-process renice map.
                    let adjustments_restored = restore_runtime_adjustments(&mut runtime).await;
                    let outcome = apply_policy_reload(
                        guard,
                        &mut runtime,
                        new_policy.guard,
                        adjustments_restored,
                    );
                    if outcome == ReloadOutcome::Deferred {
                        // The previous policy stays live on purpose; see
                        // apply_policy_reload for why adopting the new one
                        // here could strand a process adjustment.
                        eprintln!(
                            "system: SIGHUP reload deferred: keeping previous policy so pending process adjustments are retried"
                        );
                        emit_event(&DraconEvent::new(
                            "system",
                            EventSeverity::Warn,
                            "guard/policy-reload",
                            "SIGHUP reload deferred: process adjustments could not all be restored, previous policy kept".to_string(),
                        ));
                        continue;
                    }
                    interval = guard.interval_secs;
                    veprintln!(
                        2,
                        "system: policy reloaded on SIGHUP (disk_warn={}%, disk_critical={}%)",
                        guard.disk_warn_percent,
                        guard.disk_critical_percent
                    );
                    // FIXED 2026-09-27 (audit F93): `enabled` was only ever
                    // checked once, at daemon startup (main.rs:6996), so a
                    // SIGHUP that loaded `enabled = false` assigned it to
                    // the LIVE policy and the loop kept calling
                    // `run_guard_once` forever — the documented way to stop
                    // the guard did not stop cleanup, and re-enabling
                    // required a restart. Honour it on reload, mirroring
                    // the startup path. Process adjustments were already
                    // restored above, so exiting here is clean.
                    if !guard.enabled {
                        eprintln!(
                            "guard disabled in policy — stopping after SIGHUP reload (restart the service to re-enable)"
                        );
                        break;
                    }
                }
                Err(e) => {
                    // CHANGED 2026-09-09 (audit F42): the old message
                    // claimed "using defaults" but this arm assigns
                    // nothing — the previous (known-good) policy stays
                    // live, which is the safer behavior. Say so.
                    eprintln!(
                        "system: SIGHUP reload warning: corrupted policy file, keeping previous policy: {}",
                        e
                    );
                    emit_event(&DraconEvent::new(
                        "system",
                        EventSeverity::Error,
                        "guard/policy-reload",
                        format!(
                            "SIGHUP reload: policy corrupted, keeping previous policy: {}",
                            e
                        ),
                    ));
                }
            }
        }
        if let Err(e) = run_guard_once(guard, &mut runtime).await {
            eprintln!("guard pass failed: {}", e);
            emit_event(&DraconEvent::new(
                "system",
                EventSeverity::Error,
                "guard",
                format!("pass failed: {e}"),
            ));
        }
        // Registered managed jobs carry their own lifetime OOM debt. The
        // independent recoverer owns it; the daemon only observes progress so
        // a stuck recovery stays visible instead of silently drifting.
        match managed_oom::drain(false, &parse_kinds(&guard.process_exempt_names)).await {
            Ok(0) => {}
            Ok(pending) => {
                eprintln!("managed OOM recovery: {pending} registered job debt(s) outstanding")
            }
            Err(e) => eprintln!("managed OOM recovery check failed: {e:#}"),
        }
        // FIXED 2026-07-26 (audit H-12): `elapsed` must reset EVERY pass.
        // Previously declared once before the outer loop, so after the
        // first interval the inner sleep loop never ran again — the daemon
        // busy-looped guard passes back-to-back forever (continuous
        // df/ps/du + walkdir scans).
        let mut elapsed = 0u64;
        while !shutdown.load(Ordering::SeqCst) && elapsed < interval {
            sleep(Duration::from_secs(1)).await;
            elapsed += 1;
        }
    }
    if !restore_runtime_adjustments(&mut runtime).await {
        eprintln!("⚠ shutdown: some process adjustments could not be restored; durable obligations retained");
    }
    persist_mitigations(&mut runtime);
    veprintln!(1, "system: guard daemon shutdown complete");
    Ok(())
}

pub(crate) async fn cmd_guard_prune(
    guard: &GuardPolicy,
    json: bool,
    docker: bool,
    docker_volumes: bool,
    package_caches: bool,
    apply: bool,
) -> Result<()> {
    let mut reclaimed_total = 0u64;
    let mut actions = Vec::new();

    if docker || docker_volumes {
        if apply {
            match docker_prune(
                apply,
                docker,
                docker_volumes,
                guard.cleanup_command_timeout_secs,
            )
            .await
            {
                Ok(bytes) => {
                    actions.push(format!("Docker prune: {}", human_bytes(bytes)));
                    reclaimed_total += bytes;
                }
                Err(e) => {
                    actions.push(format!("Docker prune failed: {}", e));
                }
            }
        } else {
            actions.push("Docker prune (dry-run, skipped)".to_string());
        }
    }

    if package_caches {
        match clean_package_caches(true, true, true, true, apply).await {
            Ok((bytes, cleaned)) => {
                for c in cleaned {
                    actions.push(format!("Package cache: {}", c));
                }
                reclaimed_total += bytes;
            }
            Err(e) => {
                actions.push(format!("Package cache cleanup failed: {}", e));
            }
        }
    }

    if !docker && !docker_volumes && !package_caches {
        // ADDED 2026-09-09 (audit F45): this branch printed human-readable
        // disk text even with --json, and execution then fell through to
        // the JSON report below — mixed output. Emit a zeroed JSON report
        // instead.
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "reclaimed_bytes": 0,
                    "reclaimed_human": human_bytes(0),
                    "actions": Vec::<String>::new(),
                })
            );
            return Ok(());
        }
        let disk = disk_use_percent_for(&guard.disk_mount_path).await?;
        println!("Disk usage: {}% (mount: {})", disk, guard.disk_mount_path);

        if let Ok((total, used, _free)) = get_inode_info(&guard.disk_mount_path).await {
            let pct = used.saturating_mul(100).checked_div(total).unwrap_or(0) as u8;
            println!("Inode usage: {}% ({}/{} inodes used)", pct, used, total);
        }

        println!();
        println!("Potential cleanup targets:");
        println!("  --docker          Prune unused Docker images/containers");
        println!("  --docker-volumes  Prune Docker volumes too (aggressive)");
        println!("  --package-caches  Clean cargo/npm/pip/go caches");
        println!();
        println!("Add --apply to execute cleanup.");
    }

    if json {
        #[derive(Serialize)]
        struct PruneReport {
            reclaimed_bytes: u64,
            reclaimed_human: String,
            actions: Vec<String>,
        }
        let report = PruneReport {
            reclaimed_bytes: reclaimed_total,
            reclaimed_human: human_bytes(reclaimed_total),
            actions,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if !actions.is_empty() {
        println!("Prune results:");
        for a in &actions {
            println!("  - {}", a);
        }
        println!("Total reclaimed: {}", human_bytes(reclaimed_total));

        if !apply && (docker || docker_volumes || package_caches) {
            println!();
            println!("Note: This was a dry-run. Add --apply to execute.");
        }
    }

    Ok(())
}

/// Represents which cleanup targets are enabled.
#[derive(Debug, Clone, Default)]
pub(crate) struct CleanTargets {
    pub(crate) rust: bool,
    pub(crate) trash: bool,
    pub(crate) nix: bool,
    pub(crate) caches: bool,
    pub(crate) node_modules: bool,
    pub(crate) docker: bool,
    /// ADDED 2026-10-27: orphaned git temp packs (the 33 GiB class).
    pub(crate) git_temp: bool,
    /// ADDED 2026-10-27: dead swapfiles + stray dirs/bundles. Report-only by
    /// design; these are never deleted by this tool.
    pub(crate) report_only: bool,
}

impl CleanTargets {
    fn all() -> Self {
        Self {
            rust: true,
            trash: true,
            nix: true,
            caches: true,
            node_modules: true,
            docker: true,
            git_temp: true,
            report_only: true,
        }
    }

    /// Returns true if no targets are enabled.
    fn is_empty(&self) -> bool {
        !self.rust
            && !self.trash
            && !self.nix
            && !self.caches
            && !self.node_modules
            && !self.docker
            && !self.git_temp
            && !self.report_only
    }
}

/// Resolve target selection, defaulting a bare `guard clean` to all targets.
pub(crate) fn resolve_clean_targets(all: bool, targets: &CleanTargets) -> CleanTargets {
    if all || targets.is_empty() {
        CleanTargets::all()
    } else {
        targets.clone()
    }
}

pub(crate) async fn cmd_guard_clean(
    guard: &GuardPolicy,
    json: bool,
    apply: bool,
    all: bool,
    targets: CleanTargets,
    min_size_mb: Option<u64>,
) -> Result<()> {
    let targets = resolve_clean_targets(all, &targets);
    let do_rust = targets.rust;
    let do_trash = targets.trash;
    let do_nix = targets.nix;
    let do_caches = targets.caches;
    let do_node = targets.node_modules;
    let do_docker = targets.docker;
    let do_git_temp = targets.git_temp;
    let do_report_only = targets.report_only;

    let mut guard_clone = guard.clone();
    if let Some(mb) = min_size_mb {
        guard_clone.cleanup_min_size_mb = mb;
    }

    let mut total_reclaimed = 0u64;
    let mut actions: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    if do_rust {
        let mut runtime = GuardRuntimeState::default();
        // FIXED 2026-09-27 (audit F92): this passed
        // `bypass_age_gate = false`, so the `rust_target_action_min_age_days`
        // (default 7) freshness gate also applied to the operator's EXPLICIT
        // `guard clean --rust --apply`. On a full disk the freshly-built
        // `target/` that is actually consuming the space is exactly the one
        // the gate protects, so the documented "execute cleanup" flow printed
        // `Protected: … (touched 0d ago, under action min-age 7d)` and
        // reclaimed nothing — the escape hatch silently did nothing.
        //
        // The gate exists to stop AUTONOMOUS daily thrash (a deleted target
        // gets rebuilt within hours, pushing the disk back over the action
        // line), not as a safety invariant: the live-build protections
        // (mtime<60s backstop, active cargo/rustc detection, ancestor-aware
        // protected-project matching) all still apply here, exactly as they
        // do when the daemon lifts the gate at the critical tier.
        let result = auto_cleanup_rust_targets(&guard_clone, &mut runtime, apply, true).await?;
        total_reclaimed += result.reclaimed_bytes;
        for p in result.cleaned_paths {
            actions.push(format!("Rust: {}", p));
        }
        for p in result.protected_paths {
            actions.push(format!("Protected: {}", p));
        }
    }

    if do_trash {
        match empty_trash(
            apply,
            &guard_clone.protected_paths,
            guard_clone.trash_credential_guard,
            guard_clone.trash_min_age_days,
        )
        .await
        {
            Ok((bytes, cleaned)) => {
                total_reclaimed += bytes;
                for c in cleaned {
                    actions.push(format!("Trash: {}", c));
                }
            }
            Err(e) => failures.push(format!("Trash: {}", e)),
        }
    }

    if do_nix {
        match clean_nix_garbage(
            guard_clone.nix_keep_generations,
            apply,
            guard_clone.cleanup_command_timeout_secs,
        )
        .await
        {
            Ok((bytes, cleaned)) => {
                total_reclaimed += bytes;
                for c in cleaned {
                    actions.push(format!("Nix: {}", c));
                }
            }
            Err(e) => failures.push(format!("Nix: {}", e)),
        }
    }

    if do_node {
        let roots: Vec<PathBuf> = guard_clone
            .node_modules_search_roots
            .split(',')
            .filter_map(|s| {
                let s = s.trim();
                if s.is_empty() {
                    return None;
                }
                let p = expand_tilde(s);
                if p.exists() {
                    Some(p)
                } else {
                    None
                }
            })
            .collect();
        // Explicit `guard clean` always deletes (no stash-first).
        match clean_old_node_modules(
            &roots,
            guard_clone.node_modules_max_age_days,
            apply,
            &guard_clone.protected_paths,
            false,
            Path::new(""),
        )
        .await
        {
            Ok(outcome) => {
                total_reclaimed += outcome.reclaimed;
                for c in outcome.cleaned {
                    actions.push(format!("Node: {}", c));
                }
                // ADDED 2026-10-24 (audit): a protected tree is not
                // reclaimable, and the refusal only ran when applying.
                for p in outcome.protected {
                    actions.push(format!("Protected: {}", p));
                }
            }
            Err(e) => failures.push(format!("Node: {}", e)),
        }
    }

    if do_caches {
        match clean_package_caches(true, true, true, true, apply).await {
            Ok((bytes, cleaned)) => {
                total_reclaimed += bytes;
                for c in cleaned {
                    actions.push(format!("Cache: {}", c));
                }
            }
            Err(e) => failures.push(format!("Cache: {}", e)),
        }
    }

    if do_docker {
        // CHANGED 2026-09-09 (audit F44): the old call passed `apply`
        // as docker's `--all`, so EVERY `--docker --apply` deleted ALL
        // unused images with no opt-out, while the prune path tied
        // `--all` to its own flag. Tie docker `--all` to clean's `--all`
        // targets flag instead: `clean --all --apply` prunes aggressively,
        // `clean --docker --apply` prunes dangling-only.
        match docker_prune(
            apply,
            all,
            guard_clone.docker_prune_volumes,
            guard_clone.cleanup_command_timeout_secs,
        )
        .await
        {
            Ok(bytes) => {
                total_reclaimed += bytes;
                if bytes > 0 {
                    actions.push(format!("Docker: {}", human_bytes(bytes)));
                }
            }
            Err(e) => failures.push(format!("Docker: {}", e)),
        }
    }

    // Every scan root goes through expand_tilde: the live policy stores
    // "~/Dev", and PathBuf::from("~/Dev") is a literal directory named "~".
    // The first version of this block skipped that step and reported "Nothing
    // to clean" on a host holding 33 GiB of git scratch.
    let scan_roots: Vec<PathBuf> = guard_clone
        .rust_search_roots
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            (!s.is_empty()).then(|| expand_tilde(s))
        })
        .collect();

    if do_git_temp {
        let roots = &scan_roots;
        let mut candidates = Vec::new();
        for root in roots {
            candidates.extend(find_git_temp_files(root, 4));
        }
        // Per-repo ownership: only a git process working inside THIS repository
        // can be writing its scratch files. A host-wide "any git is running"
        // rule would block every pass forever, since the sync daemon commits
        // continuously.
        let owners = live_git_owners(Path::new("/proc"));
        let owner_of = |candidate: &Path| -> Option<String> {
            let repo = repo_root_for_git_path(candidate)?;
            git_owner_for(&owners, &repo).map(|owner| format!("{}(pid {})", owner.name, owner.pid))
        };
        let outcome = clean_git_temp_files(
            candidates,
            guard_clone.git_temp_min_age_hours,
            owner_of,
            apply,
        );
        total_reclaimed += outcome.reclaimed_bytes;
        for f in &outcome.cleaned {
            actions.push(format!(
                "GitTemp{}: {} ({})",
                if apply { "" } else { " (would delete)" },
                f.path.display(),
                human_bytes(f.bytes)
            ));
        }
        for f in &outcome.skipped_owned {
            let owner = repo_root_for_git_path(&f.path)
                .and_then(|repo| {
                    git_owner_for(&owners, &repo).map(|o| format!("{}(pid {})", o.name, o.pid))
                })
                .unwrap_or_else(|| "a live git process".to_string());
            actions.push(format!(
                "Skipped (live {} is working in this repo): {} ({})",
                owner,
                f.path.display(),
                human_bytes(f.bytes)
            ));
        }
        for f in &outcome.skipped_fresh {
            actions.push(format!(
                "Protected (under git_temp_min_age_hours={}h): {} ({})",
                guard_clone.git_temp_min_age_hours,
                f.path.display(),
                human_bytes(f.bytes)
            ));
        }
    }

    if do_report_only {
        // REPORT-ONLY by design: dead swapfiles, wrong-swap placement and
        // unreferenced swapfiles, plus stray dirs/bundles, are surfaced
        // with sizes and an exact command, never deleted. See
        // space_report.rs for why none of these classes is safe to automate.
        let active = std::fs::read_to_string("/proc/swaps")
            .map(|contents| active_swap_paths(&contents))
            .unwrap_or_default();
        let swap_candidates: Vec<PathBuf> = guard_clone
            .dead_swapfile_candidates
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(expand_tilde)
            .collect();
        for candidate in &swap_candidates {
            let path = candidate.clone();
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if let Some(dead) = classify_swapfile(&path, metadata.len(), &active) {
                actions.push(format!(
                    "REPORT-ONLY dead swapfile: {} ({}) — run: {}",
                    dead.path.display(),
                    human_bytes(dead.bytes),
                    dead.removal_command()
                ));
            }
        }
        // ADDED 2026-10-27 (wrong-swap check): active swap outside the
        // expected devices, and on-disk swapfiles with no NixOS
        // swapDevices entry. Same shared pass the disk audit uses.
        let expected: Vec<String> = guard_clone
            .expected_swap_prefixes
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let swap_config = expand_tilde(&guard_clone.nixos_swap_config);
        let swap_health = check_swap_health(&expected, &swap_candidates, &swap_config);
        for line in swap_health.describe() {
            actions.push(format!("REPORT-ONLY {line}"));
        }
        if !swap_health.config_known {
            actions.push(format!(
                "REPORT-ONLY swap reference check skipped (NixOS config unreadable or no swapDevices block: {})",
                swap_config.display()
            ));
        }
        let stray_roots: Vec<PathBuf> = guard_clone
            .stray_scan_roots
            .split(',')
            .filter_map(|s| {
                let s = s.trim();
                (!s.is_empty()).then(|| expand_tilde(s))
            })
            .collect();
        for root in &stray_roots {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                // Size is measured for report purposes only; a directory that
                // cannot be walked is reported at 0 rather than skipped, so the
                // operator still sees the name.
                let bytes = if file_type.is_dir() {
                    crate::dir_allocated_bytes(&entry.path(), 3, 0)
                } else {
                    entry.metadata().map(|m| m.len()).unwrap_or(0)
                };
                if let Some(item) = classify_stray(&entry.path(), &name, bytes, file_type.is_dir())
                {
                    actions.push(format!(
                        "REPORT-ONLY {}: {} ({}) — needs explicit approval",
                        item.kind.label(),
                        item.path.display(),
                        human_bytes(item.bytes)
                    ));
                }
            }
        }
    }

    if json {
        #[derive(Serialize)]
        struct CleanReport {
            reclaimed_bytes: u64,
            reclaimed_human: String,
            actions: Vec<String>,
            failures: Vec<String>,
            apply: bool,
        }
        let report = CleanReport {
            reclaimed_bytes: total_reclaimed,
            reclaimed_human: human_bytes(total_reclaimed),
            actions,
            failures,
            apply,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    // FIXED 2026-09-27 (audit rework round 4, F84): the two nested
    // `else { if .. }` wrappers are `collapsible_else_if` on the pinned
    // MSRV toolchain. Both collapsed; behaviour is identical.
    } else if actions.is_empty() && failures.is_empty() {
        println!("Nothing to clean.");
    } else {
        if !failures.is_empty() {
            eprintln!("⚠️ {} cleanup step(s) failed:", failures.len());
            for f in &failures {
                eprintln!("  • {}", f);
            }
            println!();
        }
        println!(
            "Cleanup {}:",
            if apply {
                "results"
            } else {
                "preview (dry-run)"
            }
        );

        for a in &actions {
            println!("  • {}", a);
        }
        println!();
        println!("Total reclaimable: {}", human_bytes(total_reclaimed));
        if !apply {
            println!("Add --apply to execute cleanup.");
        }
    }

    Ok(())
}

pub(crate) async fn cmd_guard(cmd: GuardCommands) -> Result<()> {
    let (_, policy) =
        load_system_policy().map_err(|error| anyhow::Error::new(PolicyLoadError(error)))?;
    let mut guard = policy.guard;
    normalize_guard_policy(&mut guard);
    // ONCE per process, before any subcommand runs: a configured stash or
    // cold root that the shipped unit leaves read-only would otherwise surface
    // only as a per-candidate EROFS, once per pass, forever.
    crate::warn_uncovered_storage_roots(&guard);
    match cmd {
        GuardCommands::Once { json } => cmd_guard_once(&guard, json).await,
        GuardCommands::Daemon => cmd_guard_daemon(&mut guard).await,
        GuardCommands::ReapPreview { json } => cmd_guard_reap_preview(&guard, json),
        GuardCommands::NotifyTest { title, body } => {
            let title = title.unwrap_or_else(|| "dracon-system test".to_string());
            let body = body.unwrap_or_else(|| {
                "notify_command works — flags are passed as argv, no shell involved".to_string()
            });
            // Report what will run, so a misconfiguration is visible here
            // rather than at the next real alert.
            match NotifyCommand::parse(&guard.notify_command) {
                Ok(cmd) => {
                    let mut rendered = cmd.program.clone();
                    for arg in &cmd.args {
                        rendered.push(' ');
                        rendered.push_str(arg);
                    }
                    println!("notify_command: {rendered}");
                    println!("title: {title}");
                    println!("body:  {body}");
                }
                Err(e) => anyhow::bail!("notify_command rejected: {e}"),
            }
            // Force the send regardless of `notify`, so an operator can test
            // a command before enabling it.
            let mut test_guard = guard.clone();
            test_guard.notify = true;
            send_notification(&test_guard, &title, &body).await;
            Ok(())
        }
        GuardCommands::Prune {
            json,
            docker,
            docker_volumes,
            package_caches,
            apply,
        } => cmd_guard_prune(&guard, json, docker, docker_volumes, package_caches, apply).await,
        GuardCommands::Clean {
            json,
            apply,
            rust,
            trash,
            nix,
            caches,
            node_modules,
            docker,
            all,
            min_size_mb,
            git_temp,
            report_only,
        } => {
            let targets = CleanTargets {
                rust,
                trash,
                nix,
                caches,
                node_modules,
                docker,
                git_temp,
                report_only,
            };
            cmd_guard_clean(&guard, json, apply, all, targets, min_size_mb).await
        }
    }
}
