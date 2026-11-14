//! The memory-pressure pass: classification, offender selection, and the reversible mitigations it applies.
//!
//! Split out of main.rs 2026-11-14 (audit task list, P3). Moved verbatim;
//! items are `pub(crate)` and re-exported from main.rs.

use super::*;

/// Memory/swap pressure guard (ADDED 2026-08-10, v0.112.35).
/// Detects the 2026-08-09/10 failure mode: RAM exhausted, swap
/// thrashing (PSI full avg10 high), everything crawling while
/// kswapd spins. Reports the top RSS offenders and, when configured,
/// applies reversible renice, OOM-bias, and CPUQuota mitigation; it
/// never kills a process itself.
pub(crate) async fn check_memory_pressure(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
    all_processes: &[ProcSample],
) -> Option<MemoryReport> {
    if !guard.monitor_memory {
        return None;
    }
    let sample = memory_sample().await?;
    let mem_available_percent = sample.mem_available_percent();
    let swap_used_percent = sample.swap_used_percent();
    let psi_full_avg10 = psi_full_avg10().await;

    // Swap-in velocity (pages/s), measured on EVERY pass. This used to be
    // a fallback computed only when PSI was unavailable — but violent
    // swap-in with moderate PSI is exactly what thrashing looks like
    // (2026-10-04: si 100k+/s at PSI 3.9 read "ok" and no mitigation
    // fired). Swap-OUT is deliberately not a signal: paging out is
    // reclaim working, not pressure; faulting back IN is the stall.
    let mut pswpin_rate = None;
    if let Some((pin, pout)) = vmstat_swap_counters().await {
        if let Some((prev_at, prev_pin, _prev_pout)) = state.prev_swap_counters {
            let dt = prev_at.elapsed().as_secs_f64();
            if dt > 0.0 {
                pswpin_rate = Some(pin.saturating_sub(prev_pin) as f64 / dt);
            }
        }
        record_swap_counters(state, pin, pout);
    }

    let mem_low = mem_available_percent <= guard.mem_available_warn_percent;
    // Swap occupancy is useful context, but is not active pressure on its
    // own: Linux may keep cold pages in swap while RAM and PSI are healthy.
    let swap_high = swap_used_percent >= guard.swap_used_warn_percent;
    let psi_thrash = psi_full_avg10.is_some_and(|v| v >= guard.mem_psi_full_warn)
        || swapin_velocity_thrash(pswpin_rate, guard.mem_swapin_warn_pages_per_sec);
    let observed_pressure = classify_memory_pressure(mem_low, swap_high, psi_thrash);
    let (pressure, _previous_pressure, _pressure_changed) = stabilize_memory_pressure_at(
        state,
        observed_pressure,
        guard.memory_pressure_sustain_secs,
        Instant::now(),
    );

    // A user service can often lower a process's priority but cannot raise it
    // again during recovery. Do not apply a reversible limiter unless the
    // service can complete both halves of that lifecycle.
    let can_restore_nice = if guard.auto_renice_on_memory || !state.memory_reniced_pids.is_empty() {
        nice_restore_capability_available(state)
    } else {
        true
    };

    // Swap is already sampled for every identity-verified process in the
    // existing process-table pass. Never discard low-RSS rows before that
    // measurement: the biggest swapped offender may be mostly nonresident.
    let exempt = parse_kinds(&guard.process_exempt_names);
    let session_names = parse_kinds(&guard.agent_session_names);
    let sessions = state
        .session_exempt_pids
        .as_ref()
        .map(|pids| SessionExemption {
            names: &session_names,
            pids,
        });
    let (offenders, swap_samples_unavailable) =
        memory_pressure_offenders_with_sessions(all_processes, &exempt, sessions);
    let top_rss: Vec<ProcSample> = offenders.into_iter().map(load_process_cmdline).collect();
    // ADDED 2026-11-08: the offenders list excludes exempt processes BY
    // POLICY, so name the exempt consumers' footprint too. Nothing may be
    // done about them, but the operator must be able to see them.
    let exempt_top = exempt_consumer_footprint_with_sessions(all_processes, &exempt, sessions);
    let mitigation_note = build_mitigation_note(&top_rss, &exempt_top);

    // ── Limiting (ADDED 2026-08-10, v0.112.36) ──────────────────
    // Deprioritize (never kill) the offenders while pressure lasts;
    // restore when it drops. All mechanisms are per-process,
    // reversible, and skip whitelisted/kernel pids. Memory is NOT
    // capped: a memory cap frees nothing and only kills (MemoryMax)
    // or freezes (MemoryHigh) the offender — renice fixes the
    // responsiveness symptom, oom_score_adj steers the kill the
    // kernel would do anyway, and CPUQuota throttles a stuck busy-
    // loop that renice can't tame. See the user discussion
    // 2026-08-10 in AGENTS.md.
    let mut limited: Vec<String> = Vec::new();
    if !state.mitigation_ledger_unavailable
        && pressure != "ok"
        && guard.auto_renice_on_memory
        && can_restore_nice
    {
        for p in &top_rss {
            let identity_ok = process_sample_is_current(p);
            if !identity_ok {
                continue;
            }
            let current_identity = process_sample_identity(p);
            drop_stale_nice_adjustments(state, p.pid, &current_identity);
            let nice_val = memory_offender_nice_value(p, guard.renice_value);
            let applied_nice = state
                .memory_reniced_pids
                .get(&p.pid)
                .map(|entry| entry.applied_nice);
            if applied_nice != Some(nice_val) {
                let original_nice = state
                    .memory_reniced_pids
                    .get(&p.pid)
                    .map(|entry| entry.original_nice)
                    .or_else(|| {
                        state
                            .reniced_pids
                            .get(&p.pid)
                            .map(|entry| entry.original_nice)
                    })
                    .unwrap_or(p.nice);
                // ADDED 2026-10-24: never apply a priority the guard could
                // not take back. The old code checked the capability in the
                // guard's OWN user namespace only, and that check said yes
                // for processes the guard provably cannot restore — so the
                // mitigation was a one-way ratchet that left the operator's
                // long-lived processes stuck at a degraded priority.
                if !can_restore_nice_for(Path::new("/proc"), p.pid) {
                    // CHANGED 2026-11-14: ONE diagnostic per daemon lifetime
                    // (the AGENTS.md contract). Per-pid logging produced
                    // ~1000 lines/week because pids churn, and the set grew
                    // without bound; it now holds at most the first pid.
                    if state.nice_restore_skipped_pids.is_empty() {
                        state.nice_restore_skipped_pids.insert(p.pid);
                        eprintln!(
                            "⚠️ renice mitigation skipped for pid={} cmd={} — the guard cannot restore its priority in that process's user namespace, so it will not lower it (further skips are not logged; consider auto_renice_on_memory = false)",
                            p.pid, p.command
                        );
                    }
                    continue;
                }
                match renice_process(p.pid, nice_val).await {
                    Ok(()) => {
                        state.memory_reniced_pids.insert(
                            p.pid,
                            MemoryReniceState {
                                original_nice,
                                applied_nice: nice_val,
                                identity: current_identity,
                            },
                        );
                        eprintln!(
                            "🛡️ mem-renice pid={} cmd={} -> nice {} (pressure {})",
                            p.pid, p.command, nice_val, pressure
                        );
                        limited.push(format!("renice {}={}", p.command, nice_val));
                    }
                    Err(e) => eprintln!(
                        "⚠️ mem-renice failed for pid={} cmd={}: {}",
                        p.pid, p.command, e
                    ),
                }
            }
        }
    }
    if pressure == "critical" && guard.bias_oom_on_pressure {
        for p in &top_rss {
            // Registered managed jobs carry their own lifetime debt and an
            // independent recoverer. Ancestry-based bias must not be applied
            // on top of it, and unregistered jobs stay explicitly unsupported.
            if managed_oom::is_managed_job_process(p.pid) {
                match managed_oom::bias(p, &exempt).await {
                    Ok(Some((orig, target))) => {
                        eprintln!(
                            "🛡️ managed oom-bias pid={} cmd={} adj {} -> {} (critical pressure)",
                            p.pid, p.command, orig, target
                        );
                        limited.push(format!("oom-bias {}={}", p.command, target));
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("managed OOM bias not applied pid={}: {e}", p.pid),
                }
                continue;
            }
            match apply_oom_bias(p, all_processes, state) {
                Ok(Some((orig, target))) => {
                    eprintln!(
                        "🛡️ oom-bias pid={} cmd={} adj {} -> {} (critical pressure)",
                        p.pid, p.command, orig, target
                    );
                    limited.push(format!("oom-bias {}={}", p.command, target));
                }
                Ok(None) => {}
                Err(e) => eprintln!("OOM bias not applied pid={}: {e}", p.pid),
            }
        }
    }
    // ADDED 2026-11-14: refusals are remembered per pid to keep the log
    // quiet, but nothing ever forgot a dead pid — the map grew with pid
    // churn for the life of the daemon. Keep only live pids.
    if !state.cap_refusals.is_empty() {
        let live: HashSet<i32> = all_processes.iter().map(|p| p.pid).collect();
        state.cap_refusals.retain(|pid, _| live.contains(pid));
    }
    if !state.mitigation_ledger_unavailable
        && pressure == "critical"
        && guard.cap_offenders_cpu_percent > 0
    {
        for p in &top_rss {
            if state.capped_pids.contains_key(&p.pid) {
                continue;
            }
            // ADDED 2026-10-24 (audit): the ATTEMPT still runs every pass.
            // The cap layer's own refusals ask for exactly that — "incomplete
            // CPU lease recovered; retry next pass" — and a lease the guard
            // cannot take back cleanly is repaired by the next attempt, not
            // by waiting. What the old code got wrong was not the retry, it
            // was the LOG: it printed the identical refusal on every pass,
            // forever, for a condition (an exempt process inside the scope)
            // that no retry can fix.
            //
            // So the refusal is remembered to keep the LOG quiet while the
            // reason is unchanged, and the reason is still named in the
            // report: the operator could otherwise see the guard doing
            // nothing about a process it was trying to throttle and had no
            // way to know the attempt had even been made.
            let identity_ok = process_sample_is_current(p);
            if !identity_ok {
                continue;
            }
            match cap_cpu_process(p.pid, guard.cap_offenders_cpu_percent, &exempt).await {
                Ok((scope, orig_cgroup)) => {
                    state.capped_pids.insert(
                        p.pid,
                        (scope.clone(), orig_cgroup, process_sample_identity(p)),
                    );
                    state.cap_refusals.remove(&p.pid);
                    eprintln!(
                        "🛡️ cpu-cap pid={} cmd={} -> CPUQuota={}% (scope {})",
                        p.pid, p.command, guard.cap_offenders_cpu_percent, scope
                    );
                    limited.push(format!(
                        "cpu-cap {}={}%",
                        p.command, guard.cap_offenders_cpu_percent
                    ));
                }
                Err(e) => {
                    limited.push(format!("cpu-cap refused {}: {}", p.command, e));
                    // Log the refusal when it is NEW or when it CHANGED. A
                    // refusal that repeats with the same reason is the same
                    // news, and the report line carries it from then on.
                    if cap_refusal_is_new(state.cap_refusals.get(&p.pid), &e) {
                        eprintln!(
                            "⚠️ cpu-cap failed for pid={} cmd={}: {}",
                            p.pid, p.command, e
                        );
                    }
                    state.cap_refusals.insert(p.pid, CapRefusal { reason: e });
                }
            }
        }
    }

    // Pressure-gated orphan reap (2026-10-04): runaways whose owners died
    // ignore the 24h auto-reap floor by design, so under warn/critical
    // pressure they are reaped by orphanhood instead of age. Opt-in,
    // default OFF. Blocking (SIGTERM grace sleeps), so it runs off the
    // tokio workers like the auto-reap pass.
    let mut pressure_reaped: Vec<ReapedProcess> = Vec::new();
    if (pressure == "warn" || pressure == "critical") && guard.reap_orphans_on_pressure {
        let policy = reap_policy_from_guard(guard);
        pressure_reaped = tokio::task::spawn_blocking(move || {
            let proc_root = Path::new("/proc");
            // No boot clock, no age evidence: fail closed like the
            // report-only scan rather than running half-blind.
            let boot_time = read_boot_time(proc_root)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(boot_time);
            let found =
                scan_pressure_orphans(proc_root, &policy, boot_time, now, PROC_TICKS_PER_SEC);
            Some(reap_pressure_orphans(
                proc_root,
                &policy,
                &found,
                PROC_TICKS_PER_SEC,
            ))
        })
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
        for r in &pressure_reaped {
            limited.push(format!(
                "pressure-reap {}={} ({})",
                r.comm,
                r.pid,
                r.outcome
                    .as_ref()
                    .map(|o| format!("{o:?}"))
                    .unwrap_or_else(|| "skipped".to_string())
            ));
        }
    }

    // ── Forgotten heavy orphan TREES (2026-10-22; trees 2026-10-23) ──
    // The operator's shape: heavy, idle, and left behind by a test — "not
    // long running and working but truly heavy and forgotten, like some
    // chrome instances left behind during tests".
    //
    // The SCAN is unconditional: every qualifying tree is reported on
    // every pass, with its evidence, and so is every tree that was
    // considered and VETOED because it looks like a live desktop session.
    // ENDING one is pressure-gated, like the other orphan path, and
    // re-verifies every gate live before the signal.
    //
    // The weight is the AGGREGATE over the tree: the 2026-10-23 leak was
    // 8.6 GB across 792 orphaned browser processes of ~3 MiB each, so the
    // per-process floor was never crossed by any of them and the guard
    // logged critical memory pressure while reaping nothing.
    let forgotten = forgotten_scan(guard);
    for c in &forgotten.candidates {
        eprintln!(
            "🧹 forgotten orphan tree: {} pid={} {} processes {}MiB (root {}MiB) age={}s cpu={}s — {}",
            c.root_comm,
            c.root_pid,
            c.member_count,
            c.aggregate_footprint_mb,
            c.root_footprint_mb,
            c.age_secs,
            c.cpu_seconds,
            c.why
        );
    }
    for v in &forgotten.vetoed {
        eprintln!(
            "🛡️ heavy process tree vetoed: {} pid={} {} processes {}MiB — {}",
            v.root_comm, v.root_pid, v.member_count, v.aggregate_footprint_mb, v.reason
        );
    }
    // A heavy orphan that is deliberately left alone is REPORTED, not
    // skipped in silence: the operator needs to see the 1.5 GB tree the
    // guard is not ending, and why.
    for p in &forgotten.protected {
        eprintln!(
            "🛡️ heavy orphan tree left alone: {} pid={} {} processes {}MiB — {}",
            p.root_comm, p.root_pid, p.member_count, p.aggregate_footprint_mb, p.reason
        );
    }
    let mut forgotten_reaped: Vec<ReapedTree> = Vec::new();
    // The gate is a NAMED function, not an inline string comparison, and it
    // fails CLOSED: an unrecognised pressure level is not permission to
    // end a process. `forgotten::forgotten_reap_allowed_under_pressure`.
    if forgotten_reap_allowed_under_pressure(&pressure) {
        let policy = reap_policy_from_guard(guard);
        let trees = forgotten.candidates.clone();
        forgotten_reaped = tokio::task::spawn_blocking(move || {
            reap_forgotten_orphan_trees(Path::new("/proc"), &policy, &trees, PROC_TICKS_PER_SEC)
        })
        .await
        .unwrap_or_default();
        for r in &forgotten_reaped {
            limited.push(format!(
                "forgotten-reap tree {}={} ({} processes, {}MiB, {} ended)",
                r.root_comm, r.root_pid, r.member_count, r.aggregate_footprint_mb, r.members_ended
            ));
        }
        // A leaked browser is two halves: the processes and the profile
        // directory they owned. The guard has just proved the browser is
        // dead, so the profile is reclaimed NOW rather than waiting out the
        // tmp age floor. Only a tree that was fully ended yields a profile.
        let profile_dirs: Vec<PathBuf> = forgotten_reaped
            .iter()
            .filter_map(|r| r.profile_dir.clone())
            .collect();
        let reclaimed_profiles =
            reclaim_reaped_browser_profiles(Path::new("/proc"), guard, &profile_dirs).await;
        for p in &reclaimed_profiles {
            limited.push(format!("reclaimed browser profile {p}"));
        }
    }

    // A child forked after its parent was biased inherits oom_score_adj=250,
    // but is not present in `oom_biased_pids`. Sweep those descendants on
    // every pass, including before a tracked parent is released or removed.
    let sweep = sweep_stranded_oom_descendants(Path::new("/proc"), all_processes, state, &exempt);
    if sweep.deferred > 0 {
        eprintln!(
            "⚠️ retaining {} pending descendant OOM restorations",
            sweep.deferred
        );
    }
    limited.extend(sweep.restored);

    // FIXED 2026-10-01 (audit): the oom_score_adj and CPUQuota RELEASES sat inside
    // `if pressure == "ok" && can_restore_nice`, but their APPLIES are not gated by
    // that flag (only the renice apply is). On a host without CAP_SYS_NICE — a
    // manual `guard daemon` run, or a unit predating v0.112.39 — the guard biased
    // oom to 250 and capped CPU under critical pressure and could then never
    // release either, while the only warning printed said "renice mitigation
    // disabled". The releases now run whenever pressure recovers, and only the
    // renice half stays gated, which is the flag that actually governs a
    // reversible renice.
    if pressure == "ok" {
        if !state.mitigation_ledger_unavailable {
            // Shared by all three release halves below: the renice, oom and CPU
            // cooldown stamps are all measured from the same release window.
            let now = Instant::now();
            let release_dur = Duration::from_secs(guard.release_after_secs);
            if can_restore_nice {
                // Un-renice memory-limited pids after the release window.
                let mut to_unrenice = Vec::new();
                for &pid in state.memory_reniced_pids.keys() {
                    let cooled_at = state.memory_cooled_since.entry(pid).or_insert(now);
                    if now.duration_since(*cooled_at) >= release_dur {
                        to_unrenice.push(pid);
                    }
                }
                for pid in to_unrenice {
                    let (original_nice, identity) = match state.memory_reniced_pids.get(&pid) {
                        Some(entry) => (entry.original_nice, entry.identity.clone()),
                        None => continue,
                    };
                    let restore_nice = state
                        .reniced_pids
                        .get(&pid)
                        .filter(|entry| same_process_incarnation(&entry.identity, &identity))
                        .map(|entry| entry.applied_nice)
                        .unwrap_or(original_nice);
                    match process_identity_status(Path::new("/proc"), pid, &identity) {
                        ProcessIdentityStatus::Match => {
                            drop_stale_nice_adjustments(state, pid, &identity);
                        }
                        ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch => {
                            remove_memory_renice(state, pid);
                            continue;
                        }
                        ProcessIdentityStatus::Unavailable => {
                            // Bounded defer: only retry the identity read a few times.
                            // Unbounded deferral is what caused the long-running memory
                            // leak when PIDs are in cgroups we cannot inspect.
                            let attempts = state
                                .memory_identity_unavailable_attempts
                                .entry(pid)
                                .or_insert(0);
                            *attempts += 1;
                            if *attempts >= IDENTITY_UNAVAILABLE_RETRY_LIMIT {
                                eprintln!(
                                "⚠️ mem-unrenice dropping pid={} after {} unavailable identity reads (will not retry)",
                                pid, *attempts
                            );
                                state.memory_identity_unavailable_attempts.remove(&pid);
                                remove_memory_renice(state, pid);
                            } else {
                                eprintln!(
                                "⚠️ mem-unrenice deferred for pid={} — process identity unavailable (attempt {}/{})",
                                pid, *attempts, IDENTITY_UNAVAILABLE_RETRY_LIMIT
                            );
                            }
                            continue;
                        }
                    }
                    // Identity was Match (or fell through after dropping) — clear any
                    // prior unavailability counter for this pid.
                    state.memory_identity_unavailable_attempts.remove(&pid);
                    if let Err(e) = renice_process(pid, restore_nice).await {
                        // Bounded defer: a renice that fails with Permission denied will
                        // fail again every cycle. Retrying forever accumulates an entry
                        // per PID in `memory_reniced_pids` and leaks memory until the
                        // cgroup's MemoryMax trips.
                        let attempts = state.memory_unrenice_failures.entry(pid).or_insert(0);
                        *attempts += 1;
                        if *attempts >= UNRENICE_RETRY_LIMIT {
                            eprintln!(
                            "⚠️ mem-unrenice dropping pid={} after {} failed renices: {} (will not retry)",
                            pid, *attempts, e
                        );
                            state.memory_unrenice_failures.remove(&pid);
                            remove_memory_renice(state, pid);
                        } else {
                            eprintln!(
                                "⚠️ mem-unrenice failed for pid={} (attempt {}/{}): {}",
                                pid, *attempts, UNRENICE_RETRY_LIMIT, e
                            );
                        }
                        continue;
                    }
                    eprintln!(
                        "🛡️ mem-unrenice pid={} -> nice {} (pressure released)",
                        pid, restore_nice
                    );
                    state.memory_unrenice_failures.remove(&pid);
                    remove_memory_renice(state, pid);
                }
                state
                    .memory_cooled_since
                    .retain(|pid, _| state.memory_reniced_pids.contains_key(pid));
            }
            // Restore oom_score_adj after the release window.
            let mut to_unbias = Vec::new();
            for &pid in state.oom_biased_pids.keys() {
                let cooled_at = state.oom_cooled_since.entry(pid).or_insert(now);
                if now.duration_since(*cooled_at) >= release_dur {
                    to_unbias.push(pid);
                }
            }
            for pid in to_unbias {
                let (orig, identity) = match state.oom_biased_pids.get(&pid).cloned() {
                    Some(entry) => entry,
                    None => continue,
                };
                match process_identity_status(Path::new("/proc"), pid, &identity) {
                    ProcessIdentityStatus::Match => {}
                    ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch => {
                        remove_oom_bias(state, pid);
                        continue;
                    }
                    ProcessIdentityStatus::Unavailable => {
                        if record_unavailable_attempt(
                            &mut state.oom_identity_unavailable_attempts,
                            pid,
                        ) {
                            eprintln!(
                            "⚠️ oom-restore retaining pid={} after {} unavailable identity reads",
                            pid, IDENTITY_UNAVAILABLE_RETRY_LIMIT
                        );
                            // A retry limit bounds warning cadence, never proof
                            // that a live adjustment was restored.
                            state
                                .oom_identity_unavailable_attempts
                                .insert(pid, IDENTITY_UNAVAILABLE_RETRY_LIMIT);
                        } else {
                            eprintln!(
                            "⚠️ oom-restore deferred for pid={} — process identity unavailable (attempt {}/{})",
                            pid,
                            state.oom_identity_unavailable_attempts.get(&pid).copied().unwrap_or(0),
                            IDENTITY_UNAVAILABLE_RETRY_LIMIT
                        );
                        }
                        continue;
                    }
                }
                // Identity was readable: clear any prior unavailability count.
                state.oom_identity_unavailable_attempts.remove(&pid);
                if let Err(e) = oom_restore::restore(Path::new("/proc"), pid, &identity, orig) {
                    eprintln!("⚠️ oom-restore failed for pid={}: {}", pid, e);
                    continue;
                }
                eprintln!(
                    "🛡️ oom-restore pid={} adj -> {} (pressure released)",
                    pid, orig
                );
                remove_oom_bias(state, pid);
            }
            state
                .oom_cooled_since
                .retain(|pid, _| state.oom_biased_pids.contains_key(pid));
            // Lift CPUQuota scopes after the release window.
            let mut to_uncap = Vec::new();
            for &pid in state.capped_pids.keys() {
                let cooled_at = state.cap_cooled_since.entry(pid).or_insert(now);
                if now.duration_since(*cooled_at) >= release_dur {
                    to_uncap.push(pid);
                }
            }
            for pid in to_uncap {
                let (scope, orig_cgroup, identity) = match state.capped_pids.get(&pid).cloned() {
                    Some(entry) => entry,
                    None => continue,
                };
                if cpu_limit::is_lease(&scope) {
                    match cpu_limit::release(&scope, &orig_cgroup).await {
                        Ok(()) => remove_cpu_cap(state, pid),
                        Err(e) => eprintln!("CPU lease release retained {scope}: {e}"),
                    }
                    continue;
                }
                let allow_pid_move = match process_identity_status(
                    Path::new("/proc"),
                    pid,
                    &identity,
                ) {
                    ProcessIdentityStatus::Match | ProcessIdentityStatus::Gone => true,
                    ProcessIdentityStatus::Mismatch => false,
                    ProcessIdentityStatus::Unavailable => {
                        if record_unavailable_attempt(
                            &mut state.cap_identity_unavailable_attempts,
                            pid,
                        ) {
                            eprintln!(
                                "⚠️ cpu-uncap retaining pid={} after {} unavailable identity reads",
                                pid, IDENTITY_UNAVAILABLE_RETRY_LIMIT
                            );
                            // Unreadability never proves the legacy unit empty.
                            // Keep its durable recovery obligation.
                            state
                                .cap_identity_unavailable_attempts
                                .insert(pid, IDENTITY_UNAVAILABLE_RETRY_LIMIT);
                        } else {
                            eprintln!(
                            "⚠️ cpu-un cap deferred for pid={} — process identity unavailable (attempt {}/{})",
                            pid,
                            state.cap_identity_unavailable_attempts.get(&pid).copied().unwrap_or(0),
                            IDENTITY_UNAVAILABLE_RETRY_LIMIT
                        );
                        }
                        continue;
                    }
                };
                // Identity was readable: clear any prior unavailability count.
                state.cap_identity_unavailable_attempts.remove(&pid);
                let systemctl_bin = match resolve_bin_strict("systemctl") {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("cpu-uncap unavailable, skipping pid={pid}: {e:#}");
                        continue;
                    }
                };
                if let Err(e) = uncap_cpu_process_with_bin(
                    &PathBuf::from(systemctl_bin),
                    Path::new("/proc"),
                    pid,
                    &scope,
                    &orig_cgroup,
                    allow_pid_move,
                )
                .await
                {
                    eprintln!("⚠️ cpu-uncap failed for pid={} scope={}: {}", pid, scope, e);
                    continue;
                }
                remove_cpu_cap(state, pid);
            }
            state
                .cap_cooled_since
                .retain(|pid, _| state.capped_pids.contains_key(pid));
        }
    } else {
        // Pressure persists: restart any release timers from zero.
        state.memory_cooled_since.clear();
        state.oom_cooled_since.clear();
        state.cap_cooled_since.clear();
    }
    // Prune only known-gone or PID-reused entries. Identity read errors are
    // retained so a transient /proc failure cannot silently lose a limiter.
    // A recycled PID (same number, different starttime) must not keep
    // throttling an innocent new process.
    state.memory_reniced_pids.retain(|pid, entry| {
        !matches!(
            process_identity_status(Path::new("/proc"), *pid, &entry.identity),
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch
        )
    });
    let pending_oom_roots: HashSet<i32> = state
        .oom_pending_descendants
        .values()
        .map(|pending| pending.root_pid)
        .collect();
    state.oom_biased_pids.retain(|pid, (_, identity)| {
        !matches!(
            process_identity_status(Path::new("/proc"), *pid, identity),
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch
        ) || pending_oom_roots.contains(pid)
    });
    prune_oom_known_descendants(state, all_processes);
    state.capped_pids.retain(|pid, (_, _, identity)| {
        // Same rule as the memory and OOM prunes above: a recycled PID
        // (same number, different starttime) must not keep throttling an
        // innocent new process.
        !matches!(
            process_identity_status(Path::new("/proc"), *pid, identity),
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch
        )
    });

    // ADDED 2026-11-08: the transition is decided on the CLASS (ok vs
    // pressure), not the raw severity, so a warn<->critical oscillation is
    // not treated as a new condition. A genuine arrival into pressure and a
    // genuine recovery still are.
    let decision = memory_pressure_report_decision(state, &pressure, guard.report_repeat_secs);

    // Desktop notifications are reserved for stabilized state transitions.
    // An unchanged warning may still produce an occasional structured event,
    // but it will not repeatedly interrupt the operator.
    //
    // CHANGED 2026-11-08: the gate is the CLASS transition (ok -> pressure),
    // not the raw severity. The memory path never used `should_notify`, so
    // a warn<->critical oscillation produced a desktop Error on every flip
    // with no cooldown at all.
    if decision.notify {
        let offenders: String = top_rss
            .iter()
            .map(memory_offender_description)
            .collect::<Vec<_>>()
            .join(", ");
        send_notification(
            guard,
            "Dracon System Guard - Memory Pressure",
            &crate::memory_pressure_notification_body(
                &pressure,
                mem_available_percent,
                swap_used_percent,
                psi_full_avg10,
                &if offenders.is_empty() {
                    "(none)".to_string()
                } else {
                    offenders
                },
                swap_samples_unavailable,
                &mitigation_note,
            ),
        )
        .await;
    }
    // Emit on arrival, on recovery, and then only at the configured summary
    // interval while the same condition persists.
    //
    // CHANGED 2026-11-08: recovery is a CLASS transition (pressure -> ok).
    // The old comparison was on the raw severity, so a warn<->critical
    // oscillation was read as an escalation AND a recovery in alternation.
    if decision.emit_event {
        emit_event(&DraconEvent::new(
            "system",
            if pressure == "critical" {
                EventSeverity::Error
            } else if pressure == "warn" {
                EventSeverity::Warn
            } else {
                EventSeverity::Info
            },
            "memory/pressure",
            crate::memory_pressure_event_message(
                &pressure,
                observed_pressure,
                mem_available_percent,
                swap_used_percent,
                psi_full_avg10,
                &top_rss,
                swap_samples_unavailable,
                &mitigation_note,
            ),
        ));
    }

    Some(MemoryReport {
        mem_available_percent,
        swap_used_percent,
        psi_full_avg10,
        pswpin_rate,
        observed_pressure: observed_pressure.to_string(),
        pressure,
        top_rss,
        mitigation_note,
        swap_samples_unavailable,
        limited,
        pressure_reaped,
        forgotten_trees: forgotten.candidates,
        vetoed_trees: forgotten.vetoed,
        forgotten_reaped,
        protected_trees: forgotten.protected,
    })
}
