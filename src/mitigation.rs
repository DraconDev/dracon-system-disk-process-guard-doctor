//! Reversible process mitigations under memory pressure: graduated
//! renice, `oom_score_adj` bias, and the descendant restore bookkeeping.
//! The guard never kills from here — see the module comments below.
//!
//! Split out of main.rs 2026-11-14 (audit task list, P3). Moved verbatim;
//! items are `pub(crate)` and re-exported from main.rs.

use super::*;

/// Graduated auto-renice: higher CPU/memory usage = higher nice value (lower priority).
/// The process still gets full CPU when nothing else needs it — it just yields to the DE
/// and other interactive processes.
///
/// `current_nice` is part of the target calculation so an externally nicer process is
/// never raised in priority merely because its current tier maps to a smaller value.
///
/// Process mitigation is limited to reversible renice, optional `oom_score_adj`
/// biasing, and optional CPUQuota capping. The guard never invokes `kill`:
/// OOM biasing only influences the kernel's last-resort choice if an OOM occurs,
/// while CPUQuota throttles the process without killing it.
pub(crate) fn graduated_nice_value(
    cpu_percent: f32,
    footprint_mb: u64,
    base_nice: i32,
    current_nice: i32,
) -> i32 {
    let cpu_tiers: &[(f32, i32)] = &[(500.0, 15), (300.0, 10), (180.0, 5)];
    let mem_tiers: &[(u64, i32)] = &[(8192, 10), (4096, 5)];
    let cpu_nice = cpu_tiers
        .iter()
        .find(|(threshold, _)| cpu_percent >= *threshold)
        .map(|(_, nice)| *nice)
        .unwrap_or(base_nice);
    let mem_nice = mem_tiers
        .iter()
        .find(|(threshold, _)| footprint_mb >= *threshold)
        .map(|(_, nice)| *nice)
        .unwrap_or(0);
    cpu_nice.max(mem_nice).clamp(0, 19).max(current_nice)
}

/// Nice target for a pressure offender: the memory tiers run on the
/// swap-aware footprint (RSS + known swap), matching offender selection.
/// A mostly-swapped low-RSS process must still reach the graduated tiers;
/// unknown swap falls back to RSS via `memory_pressure_footprint_mb`.
pub(crate) fn memory_offender_nice_value(p: &ProcSample, base_nice: i32) -> i32 {
    graduated_nice_value(
        p.cpu_percent,
        memory_pressure_footprint_mb(p),
        base_nice,
        p.nice,
    )
}

pub(crate) async fn renice_process_with_bin(bin: &Path, pid: i32, value: i32) -> Result<()> {
    let output = Command::new(bin)
        .args(["-n", &value.to_string(), "-p", &pid.to_string()])
        .output()
        .await
        .with_context(|| format!("failed to invoke {}", bin.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.trim().is_empty() {
        anyhow::bail!("renice exited with status {}", output.status);
    }
    anyhow::bail!("renice exited with status {}: {}", output.status, stderr);
}

pub(crate) async fn renice_process(pid: i32, value: i32) -> Result<()> {
    // Never exec a bare PATH-relative name for a privilege op: resolve the
    // NixOS store path first (falls back to the bare name off NixOS, which
    // is the old behavior there).
    renice_process_with_bin(&PathBuf::from(resolve_bin_strict("renice")?), pid, value).await
}

/// OOM-killer steering target (v0.112.36). Higher oom_score_adj =
/// more likely to be picked when the kernel's last-resort OOM killer
/// fires. Writing it NEVER triggers a kill — it only steers the
/// victim choice IF the kernel kills anyway. Returns the target to
/// write, or None when the process should be left alone (already at/
/// above the bias, or deliberately protected with adj <= -500).
pub(crate) const OOM_BIAS_TARGET: i32 = 250;
pub(crate) const OOM_PROTECTED_ADJ: i32 = -500;

pub(crate) fn apply_oom_bias(
    p: &ProcSample,
    samples: &[ProcSample],
    state: &mut GuardRuntimeState,
) -> Result<Option<(i32, i32)>, String> {
    if state.mitigation_ledger_unavailable || !process_sample_is_current(p) {
        return Ok(None);
    }
    use std::io::{Read, Seek, SeekFrom, Write};
    let path = format!("/proc/{}/oom_score_adj", p.pid);
    // Hold one proc descriptor across identity checks, original-value capture,
    // durable intent and write. Reopening by PID could target a replacement.
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| format!("open {path}: {e}"))?;
    if !process_sample_is_current(p) {
        return Err("process changed after OOM descriptor open".into());
    }
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|e| e.to_string())?;
    let original = text.trim().parse::<i32>().map_err(|e| e.to_string())?;
    // FIXED 2026-10-24 (audit O1): the ledger's record says this pid is
    // already biased, and the old code trusted that WITHOUT looking at the
    // live value. A record whose live value is not the bias target is a
    // leftover — the adjustment was restored by something else — and
    // trusting it silently disabled OOM steering for a live top-RSS process:
    // the guard skipped the pid on every pass because its own bookkeeping
    // said "already done". Two live processes carried exactly that record on
    // this host when the audit found it (the ledger said "biased", the live
    // `oom_score_adj` was back at 200, and the host was at 2% available
    // memory).
    //
    // The live value decides. An unreadable value is NOT proof that the
    // claim is false, so the claim is kept.
    if state.oom_biased_pids.contains_key(&p.pid) {
        if !recorded_bias_is_stale(Some(original)) {
            return Ok(None);
        }
        eprintln!(
            "⚠️ oom-bias record for pid={} is stale (live adj {} is not the bias target) — re-applying",
            p.pid, original
        );
        state.oom_biased_pids.remove(&p.pid);
    }
    let Some(target) = oom_bias_target(original) else {
        return Ok(None);
    };
    state
        .oom_biased_pids
        .insert(p.pid, (original, process_sample_identity(p)));
    state.oom_known_descendants.insert(
        p.pid,
        process_descendant_samples(samples, p.pid)
            .into_iter()
            .map(|child| (child.pid, child.starttime))
            .collect(),
    );
    if !persist_mitigations(state) {
        remove_oom_bias(state, p.pid);
        return Err("write-ahead restoration intent could not be persisted".into());
    }
    if !process_sample_is_current(p) {
        return Err("process changed before OOM bias".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    file.write_all(format!("{target}\n").as_bytes())
        .map_err(|e| format!("write opened {path}: {e}; recovery intent retained"))?;
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    text.clear();
    file.read_to_string(&mut text).map_err(|e| e.to_string())?;
    if text.trim().parse::<i32>().ok() != Some(target) {
        return Err("OOM apply verification failed; recovery intent retained".into());
    }
    Ok(Some((original, target)))
}

pub(crate) fn oom_bias_target(current: i32) -> Option<i32> {
    if current >= OOM_BIAS_TARGET || current <= OOM_PROTECTED_ADJ {
        None
    } else {
        Some(OOM_BIAS_TARGET)
    }
}

/// Whether a recorded OOM bias no longer describes the live process.
///
/// ADDED 2026-10-24 (audit O1): a record whose live `oom_score_adj` is not
/// the bias target is a LEFTOVER — the adjustment was restored by something
/// else — and the guard must not treat the process as "already biased".
/// `None` means the live value could not be read, which is NOT proof that
/// the claim is false.
pub(crate) fn recorded_bias_is_stale(live_adj: Option<i32>) -> bool {
    live_adj.is_some_and(|live| live != OOM_BIAS_TARGET)
}

pub(crate) const KERNEL_PROCESS_PREFIXES: &[&str] = &[
    "kworker",
    "ksoftirqd",
    "kthreadd",
    "kswapd",
    "kcompactd",
    "rcu_",
    "kdevtmpfs",
    "kblockd",
    "khugepaged",
    "ksmd",
    "kernfs",
    "kauditd",
    "kstrp",
    "mm_percpu",
    "oom_reaper",
    "kvm",
    "ktrain",
    "kthrotld",
    "scsi_",
    "nvme",
    "irq/",
    "watchdog",
];

pub(crate) fn is_kernel_process(command: &str) -> bool {
    KERNEL_PROCESS_PREFIXES
        .iter()
        .any(|prefix| command.starts_with(prefix))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct OomSweepResult {
    pub(crate) restored: Vec<String>,
    pub(crate) deferred: usize,
}

pub(crate) fn remember_oom_descendant_identity(
    state: &mut GuardRuntimeState,
    root_pid: i32,
    pid: i32,
    identity: &ProcessIdentity,
) {
    state
        .oom_known_descendants
        .entry(root_pid)
        .or_default()
        .insert((pid, identity.starttime));
}

/// Retry descendant writes that were previously unreadable or failed. A
/// pending child keeps its root bias entry alive until restoration succeeds,
/// the child is proven gone, or its PID incarnation changes.
pub(crate) fn restore_pending_oom_descendants(
    proc_root: &Path,
    state: &mut GuardRuntimeState,
    exempt_names: &HashSet<String>,
) -> OomSweepResult {
    let pending_keys: Vec<(i32, u64)> = state.oom_pending_descendants.keys().copied().collect();
    let mut result = OomSweepResult::default();
    // CHANGED 2026-11-14: defunct drops are bookkeeping, not events —
    // counted and summarised once per sweep (was ~2,900 lines/week).
    let mut defunct_dropped = 0usize;
    for key in pending_keys {
        let Some(mut pending) = state.oom_pending_descendants.get(&key).cloned() else {
            continue;
        };
        match process_identity_status(proc_root, key.0, &pending.identity) {
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch => {
                state.oom_pending_descendants.remove(&key);
                continue;
            }
            ProcessIdentityStatus::Unavailable => {
                result.deferred += 1;
                if record_oom_retry(state, key, &mut pending) {
                    eprintln!("⚠️ oom-descendant-restore retaining pid={} parent={} after {} unavailable identity reads", key.0, pending.root_pid, pending.attempts);
                    state.oom_pending_descendants.insert(key, pending.clone());
                }
                continue;
            }
            ProcessIdentityStatus::Match => {}
        }
        // FIXED 2026-10-24 (audit): a zombie has a /proc entry and a matching
        // identity, so the checks above treat it as alive — but the kernel
        // refuses oom_score_adj writes for a process with no address space,
        // so the restoration can NEVER succeed against one. Retrying it
        // forever kept the pending entry alive, and a live pending entry
        // blocks the release of the biased parent (see `remove_oom_bias`).
        //
        // The debt is discharged by the child's death: there is no
        // oom_score_adj left to restore, and its children cannot inherit
        // anything from a process that no longer exists.
        if process_is_zombie(proc_root, key.0) {
            state.oom_pending_descendants.remove(&key);
            defunct_dropped += 1;
            continue;
        }
        let current_identity = match read_proc_identity(proc_root, key.0) {
            Ok(identity) => identity,
            Err(_) => {
                result.deferred += 1;
                record_oom_retry(state, key, &mut pending);
                continue;
            }
        };
        if exempt_names.contains(&current_identity.comm)
            || is_kernel_process(&current_identity.comm)
        {
            // Exemption forbids mutation, not remembering an unresolved debt.
            result.deferred += 1;
            record_oom_retry(state, key, &mut pending);
            continue;
        }
        let changed = match oom_restore::restore(
            proc_root,
            key.0,
            &pending.identity,
            pending.original_adj,
        ) {
            Ok(changed) => changed,
            Err(error) => {
                result.deferred += 1;
                if record_oom_retry(state, key, &mut pending) {
                    eprintln!("⚠️ oom-descendant-restore retaining pid={} parent={} after {} failures: {}",key.0,pending.root_pid,pending.attempts,error);
                }
                continue;
            }
        };
        if changed {
            eprintln!(
                "🛡️ oom-descendant-restore pid={} parent={} adj -> {}",
                key.0, pending.root_pid, pending.original_adj
            );
            result.restored.push(format!(
                "oom-restore-descendant {}={}",
                pending.identity.comm, pending.original_adj
            ));
        }
        state.oom_pending_descendants.remove(&key);
        remember_oom_descendant_identity(state, pending.root_pid, key.0, &pending.identity);
    }
    if defunct_dropped > 0 {
        veprintln!(
            1,
            "🧹 oom-descendant-restore: dropped {defunct_dropped} defunct descendant(s) — nothing left to restore"
        );
    }
    result
}

/// Restore oom_score_adj inherited by descendants created after a tracked
/// process was biased. Existing descendants are recorded at bias time so a
/// pre-existing operator adjustment is not overwritten. Nested biased roots
/// assign each new descendant to the nearest root, avoiding arbitrary
/// HashMap iteration order.
pub(crate) fn sweep_stranded_oom_descendants(
    proc_root: &Path,
    samples: &[ProcSample],
    state: &mut GuardRuntimeState,
    exempt_names: &HashSet<String>,
) -> OomSweepResult {
    let tracked_pids: HashSet<i32> = state.oom_biased_pids.keys().copied().collect();
    let mut root_values = HashMap::new();
    for (&pid, &(original_adj, ref identity)) in &state.oom_biased_pids {
        match process_identity_status(proc_root, pid, identity) {
            ProcessIdentityStatus::Match | ProcessIdentityStatus::Gone => {
                root_values.insert(pid, (original_adj, identity.clone()));
            }
            ProcessIdentityStatus::Mismatch | ProcessIdentityStatus::Unavailable => {}
        }
    }
    // FIXED 2026-10-24 (audit O1): a root that has been RELEASED still owns
    // restoration debts. `remove_oom_bias` no longer refuses to release a
    // root while a child restoration is pending — the debts are
    // self-contained in the pending map, which carries its own root pid and
    // original value — but a child of that root INHERITED the bias target
    // when it was forked, and the sweep could no longer attribute it to the
    // root (the root was no longer a biased root), so it stayed pinned at
    // 250 for the rest of its life.
    //
    // The pending entries carry the root's original value, so a released
    // root's original value is still known: keep sweeping against it for as
    // long as it owes a restoration.
    let mut root_original_adj: HashMap<i32, i32> = root_values
        .iter()
        .map(|(pid, (adj, _))| (*pid, *adj))
        .collect();
    for pending in state.oom_pending_descendants.values() {
        root_original_adj
            .entry(pending.root_pid)
            .or_insert(pending.original_adj);
    }
    let biased_roots: HashSet<i32> = root_original_adj.keys().copied().collect();
    let parent_by_pid: HashMap<i32, i32> = samples
        .iter()
        .map(|sample| (sample.pid, sample.ppid))
        .collect();
    for child in samples {
        if tracked_pids.contains(&child.pid)
            || exempt_names.contains(&child.command)
            || is_kernel_process(&child.command)
        {
            continue;
        }
        let Some(root_pid) = nearest_biased_ancestor(child.pid, &parent_by_pid, &biased_roots)
        else {
            continue;
        };
        let key = (child.pid, child.starttime);
        if state.oom_pending_descendants.contains_key(&key)
            || state
                .oom_known_descendants
                .get(&root_pid)
                .is_some_and(|known| known.contains(&key))
        {
            continue;
        }
        let Some(original_adj) = root_original_adj.get(&root_pid) else {
            continue;
        };
        state.oom_pending_descendants.insert(
            key,
            OomPendingDescendant {
                root_pid,
                original_adj: *original_adj,
                identity: process_sample_identity(child),
                attempts: 0,
            },
        );
    }
    // The write-ahead persist comes FIRST: a crash mid-restore must leave the
    // ledger still saying "there is a pending restoration".
    if !persist_mitigations(state) {
        return OomSweepResult {
            restored: Vec::new(),
            deferred: state.oom_pending_descendants.len(),
        };
    }
    let result = restore_pending_oom_descendants(proc_root, state, exempt_names);
    // FIXED 2026-10-24 (audit): the restore REMOVES the entries it has
    // discharged, and that removal was never persisted. The ledger therefore
    // kept dead pending entries on disk and re-hydrated them on the next
    // daemon start — the debt came back from the dead and blocked the
    // parent's release again. Persist the settled state as well.
    persist_mitigations(state);
    result
}
