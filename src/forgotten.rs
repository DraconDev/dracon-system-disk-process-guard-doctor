//! Forgotten heavy orphans — the shape the operator described as
//! "not long running and working but truly heavy and forgotten, like some
//! chrome instances left behind during tests".
//!
//! This is the opposite of the existing orphan pass in `reap.rs`. That one
//! is deliberately ignorant of age, CPU and weight: it exists to catch a
//! HOT YOUNG orphan, the thing that is still burning CPU after its test
//! runner died. This one is heavy and idle, and it only ever considers a
//! process it can show is a TEST job.
//!
//! Three rules carry the whole design:
//!
//! - **A live desktop session is never a candidate.** Not headless and no
//!   test profile means the process belongs to the operator's own session,
//!   whatever its weight. The operator's Chrome is a normal, long-running,
//!   working session; it is not "forgotten" and must never be touched.
//! - **A tree is weighed as a whole.** A browser is not one process: it is
//!   a root, a zygote, renderers, GPU and network helpers, and a detached
//!   crashpad handler. On 2026-10-23 that tree leaked 8.6 GB across 792
//!   processes whose median single-process weight was 3 MiB, so no member
//!   ever crossed a per-process floor and the guard logged critical memory
//!   pressure while doing nothing. The weight that decides is the AGGREGATE
//!   of the tree, and the tree is ended as a unit.
//! - **Reporting is unconditional; ending is pressure-gated.** The guard
//!   reports every qualifying tree with its evidence on every pass. It
//!   terminates one only under memory pressure, through the existing
//!   pressure-orphan path, and only after re-verifying every gate live
//!   (including the starttime nonce, so a recycled PID is never signalled).
//!
//! The module lives beside `reap.rs` rather than inside it because it ends
//! processes: a diagnostic that can end a process is a different tool with
//! a different blast radius, and that discipline is worth keeping visible.

use crate::reap::{
    matches_exempt, parent_comm_is_systemd, read_proc_table, ProcEntry, ProcTable, ReapPolicy,
};
use std::collections::HashSet;
use std::path::Path;

/// What a browser-ish command line is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DesktopVerdict {
    /// A test job: headless, or running from a temporary/test profile.
    Test { why: String },
    /// A live desktop session — never a candidate.
    Desktop { reason: String },
    /// Not a browser at all.
    NotABrowser,
}

/// The command-line flags that mean "this browser is a test job".
const HEADLESS_MARKERS: &[&str] = &["--headless", "headless_shell"];
/// Path fragments that mean "this profile belongs to a test harness".
const TEST_PROFILE_MARKERS: &[&str] = &[
    "playwright",
    "puppeteer",
    "chromium-",
    "chrome-",
    "headless",
    ".cache",
    "tmp",
];

/// Classify a command line: a test job, a live desktop session, or neither.
///
/// The rule is deliberately one-directional: anything that is NOT headless
/// and does NOT carry a test profile is a desktop session, whatever else it
/// looks like. A heavier, newer or stranger browser that the rules do not
/// recognise is therefore vetoed rather than risked.
pub(crate) fn classify_desktop_session(args: &str) -> DesktopVerdict {
    let lower = args.to_ascii_lowercase();
    if !lower.contains("chrom") {
        return DesktopVerdict::NotABrowser;
    }
    if HEADLESS_MARKERS.iter().any(|marker| lower.contains(marker)) {
        return DesktopVerdict::Test {
            why: "headless".to_string(),
        };
    }
    // A browser with no profile flag at all, or one pointing at the
    // operator's own profile directory, is a desktop session.
    let profile = extract_user_data_dir(&lower);
    match profile {
        None => DesktopVerdict::Desktop {
            reason: "no --user-data-dir: a normal browser session".to_string(),
        },
        Some(path) => {
            if TEST_PROFILE_MARKERS.iter().any(|m| path.contains(m)) {
                DesktopVerdict::Test {
                    why: format!("test profile {path}"),
                }
            } else {
                DesktopVerdict::Desktop {
                    reason: format!("profile {path} is not a test profile"),
                }
            }
        }
    }
}

/// The `--user-data-dir` value from a command line, if it has one.
fn extract_user_data_dir(lower_args: &str) -> Option<String> {
    let idx = lower_args.find("--user-data-dir=")?;
    let rest = &lower_args[idx + "--user-data-dir=".len()..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .unwrap_or(rest.len());
    let value = rest[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// The pressure level at which ending a forgotten orphan is permitted.
///
/// ADDED 2026-10-23: the design doc and the CHANGELOG both named this
/// function as the gate that "fails CLOSED", and both claimed a test
/// pinned the closed side by name. Neither existed: the pass compared a
/// string inline, so the property the documentation promised was not
/// enforced anywhere. It is now a named function, and it fails closed —
/// an unrecognised pressure level is NOT permission to end a process.
pub(crate) fn forgotten_reap_allowed_under_pressure(pressure: &str) -> bool {
    matches!(pressure, "warn" | "critical")
}

/// How long a tree must have been running before the guard may end it.
///
/// ADDED 2026-10-23, after a live measurement: an orphaned headless
/// Chromium root had burned only FOUR SECONDS of CPU, so the
/// lifetime-CPU gate does not distinguish "forgotten" from "just
/// detached and still in use". A browser that a harness deliberately
/// detached (setsid, double-fork) and still drives over CDP is orphaned
/// by design, headless by design, and idle-looking by design — the only
/// thing that separates it from a leak is TIME.
///
/// A leak lives for hours or days; a browser that was detached a moment
/// ago may still have a client attached. Five minutes of running time
/// separates the two shapes without delaying a real leak meaningfully:
/// the guard reaps it on the next pass after the tree ages past this.
///
/// This is a CONSTANT, not a knob: it is a safety floor, and a safety
/// floor that an operator can casually tune is not a floor. The
/// operator's escape hatch for "do not touch these trees at all" is the
/// footprint floor (`forgotten_orphan_tree_footprint_mb`) and the exempt
/// list, both of which are knobs.
pub(crate) const FORGOTTEN_MIN_TREE_AGE_SECS: u64 = 300;

/// One member of an orphaned process tree.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct TreeMember {
    pub(crate) pid: i32,
    pub(crate) comm: String,
    pub(crate) rss_mb: u64,
    pub(crate) swap_mb: u64,
    /// Resident PLUS swapped footprint, MiB.
    pub(crate) footprint_mb: u64,
    /// How far below the tree root this member sits. The tree is ended
    /// deepest-first, so a parent cannot respawn a child that is already
    /// gone.
    pub(crate) depth: u32,
    /// "root", "descendant", or "crashpad handler".
    pub(crate) role: String,
    /// `/proc/<pid>/stat` field 22 at scan time — the kill-time nonce.
    #[serde(skip_serializing)]
    pub(crate) starttime: u64,
}

/// An orphaned TEST tree, weighed as a whole.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ForgottenTree {
    /// The root's PID. The root is the member whose owner died.
    pub(crate) root_pid: i32,
    pub(crate) root_comm: String,
    pub(crate) root_args: String,
    /// Resident PLUS swapped footprint of the WHOLE tree, MiB. This is the
    /// number that decides whether the tree is heavy enough to matter.
    pub(crate) aggregate_footprint_mb: u64,
    /// The root's own footprint, MiB.
    pub(crate) root_footprint_mb: u64,
    pub(crate) member_count: usize,
    pub(crate) age_secs: u64,
    pub(crate) cpu_seconds: u64,
    /// Why the root qualifies as a TEST job: "headless" or "test profile …".
    pub(crate) why: String,
    /// Every member, root first. Ordered by depth so the tree can be
    /// ended deepest-first.
    pub(crate) members: Vec<TreeMember>,
    /// `/proc/<root_pid>/stat` field 22 at scan time — the kill-time nonce.
    #[serde(skip_serializing)]
    pub(crate) root_starttime: u64,
}

/// A tree that was weighed and left alone, with the reason.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct VetoedTree {
    pub(crate) root_pid: i32,
    pub(crate) root_comm: String,
    pub(crate) aggregate_footprint_mb: u64,
    pub(crate) member_count: usize,
    pub(crate) reason: String,
}

/// The outcome of one scan: what qualifies, and what was vetoed.
#[derive(Debug, Clone, Default)]
pub(crate) struct ForgottenTreeScan {
    pub(crate) candidates: Vec<ForgottenTree>,
    pub(crate) vetoed: Vec<VetoedTree>,
}

/// The crashpad handler PID a command line names, if any.
///
/// Chromium's crashpad handler detaches out of the browser's process tree
/// (it calls `setsid`, so it lands in a process group and session of its
/// own) and is then reparented to init when the browser dies — so it
/// survives the browser as a lone orphan. The browser's own command lines
/// still name it, which is how it is tied back to the tree it belongs to.
fn crashpad_handler_pid(args: &str) -> Option<i32> {
    const MARKER: &str = "--crashpad-handler-pid=";
    let idx = args.find(MARKER)?;
    let rest = &args[idx + MARKER.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// The weight of one member, read now.
fn member_weight(proc_root: &Path, entry: &ProcEntry) -> (u64, u64) {
    let dir = proc_root.join(entry.pid.to_string());
    let rss_mb = crate::reap::rss_mb_from_status(&dir);
    let swap_mb = crate::reap::swap_mb_from_status(&dir);
    (rss_mb, swap_mb)
}

/// Build the tree rooted at `root_pid`: the root, its descendants, and the
/// detached crashpad handler its members name.
fn build_tree(
    proc_root: &Path,
    table: &ProcTable,
    root_pid: i32,
    policy: &ReapPolicy,
    boot_time: u64,
    now: u64,
    ticks_per_sec: u64,
) -> ForgottenTree {
    let Some(root) = table.get(root_pid) else {
        unreachable!("build_tree is only called for a root that is in the table");
    };
    let mut members: Vec<TreeMember> = Vec::new();
    let mut seen: HashSet<i32> = HashSet::new();
    seen.insert(root_pid);

    let (root_rss, root_swap) = member_weight(proc_root, root);
    members.push(TreeMember {
        pid: root.pid,
        comm: root.comm.clone(),
        rss_mb: root_rss,
        swap_mb: root_swap,
        footprint_mb: root_rss.saturating_add(root_swap),
        depth: 0,
        role: "root".to_string(),
        starttime: root.starttime,
    });

    // The descendants, breadth-first, so each one knows how deep it sits.
    let mut handler_links: Vec<(i32, u32)> = Vec::new();
    for (pid, depth) in table.descendants(root_pid) {
        let Some(entry) = table.get(pid) else { continue };
        if let Some(handler) = crashpad_handler_pid(&entry.args) {
            handler_links.push((handler, depth + 1));
        }
        // An exempt member is left alone AND left out of the weight: the
        // exemption means "this is the operator's", so the guard neither
        // ends it nor counts it as reclaimable.
        if matches_exempt(&entry.exe, &policy.exempt_names) {
            continue;
        }
        if !seen.insert(pid) {
            continue;
        }
        let (rss_mb, swap_mb) = member_weight(proc_root, entry);
        members.push(TreeMember {
            pid: entry.pid,
            comm: entry.comm.clone(),
            rss_mb,
            swap_mb,
            footprint_mb: rss_mb.saturating_add(swap_mb),
            depth,
            role: "descendant".to_string(),
            starttime: entry.starttime,
        });
    }

    // The detached crashpad handler, if the tree names one and the handler
    // is itself an orphan. Never reach outside the tree: a handler that
    // still has a live parent belongs to a LIVE browser.
    for (handler_pid, depth) in handler_links {
        if seen.contains(&handler_pid) {
            continue;
        }
        let Some(handler) = table.get(handler_pid) else {
            continue;
        };
        if !parent_comm_is_systemd(proc_root, handler.ppid) {
            continue;
        }
        if matches_exempt(&handler.exe, &policy.exempt_names) {
            continue;
        }
        seen.insert(handler_pid);
        let (rss_mb, swap_mb) = member_weight(proc_root, handler);
        members.push(TreeMember {
            pid: handler.pid,
            comm: handler.comm.clone(),
            rss_mb,
            swap_mb,
            footprint_mb: rss_mb.saturating_add(swap_mb),
            depth,
            role: "crashpad handler".to_string(),
            starttime: handler.starttime,
        });
    }

    let aggregate_footprint_mb = members
        .iter()
        .fold(0u64, |acc, m| acc.saturating_add(m.footprint_mb));
    let root_footprint_mb = members.first().map(|m| m.footprint_mb).unwrap_or(0);
    let cpu_seconds = root.cpu_ticks / ticks_per_sec.max(1);
    let age_secs = crate::reap::elapsed_secs(root.starttime, ticks_per_sec, boot_time, now);
    ForgottenTree {
        root_pid: root.pid,
        root_comm: root.comm.clone(),
        root_args: root.args.clone(),
        aggregate_footprint_mb,
        root_footprint_mb,
        member_count: members.len(),
        age_secs,
        cpu_seconds,
        why: String::new(),
        members,
        root_starttime: root.starttime,
    }
}

/// The live weight of a tree, measured now: the root's own footprint and
/// the aggregate of every member.
pub(crate) fn measure_tree_footprint(
    proc_root: &Path,
    root_pid: i32,
    members: &[TreeMember],
) -> (u64, u64) {
    let mut aggregate = 0u64;
    let mut root_footprint = 0u64;
    for member in members {
        let dir = proc_root.join(member.pid.to_string());
        let rss_mb = crate::reap::rss_mb_from_status(&dir);
        let swap_mb = crate::reap::swap_mb_from_status(&dir);
        let footprint = rss_mb.saturating_add(swap_mb);
        aggregate = aggregate.saturating_add(footprint);
        if member.pid == root_pid {
            root_footprint = footprint;
        }
    }
    (root_footprint, aggregate)
}

/// Find the heavy, idle, orphaned test trees.
///
/// Every criterion below is a precondition for the ROOT, and each one
/// exists to rule out a live interactive session:
///
/// - the owner is provably dead (reparented to init/systemd),
/// - no controlling terminal (no shell, tmux pane or agent session owns it),
/// - sleeping, not working,
/// - idle by measurement: near-zero lifetime CPU,
/// - classified as a TEST job by the rule above.
///
/// The ROOT's own weight is measured against `per_process_floor_mb`, and the
/// tree's AGGREGATE weight against `tree_floor_mb`: a tree qualifies when
/// either crosses its floor. The aggregate is the half that matters — the
/// 2026-10-23 leak was 8.6 GB spread over 792 processes of ~3 MiB each,
/// so no single member ever crossed a per-process floor.
pub(crate) fn scan_forgotten_orphan_trees(
    proc_root: &Path,
    policy: &ReapPolicy,
    boot_time: u64,
    now: u64,
    ticks_per_sec: u64,
    per_process_floor_mb: u64,
    tree_floor_mb: u64,
) -> ForgottenTreeScan {
    let mut out = ForgottenTreeScan::default();
    let table = read_proc_table(proc_root);
    let self_pid = std::process::id() as i32;

    // 1. The orphan roots: owner provably dead, detached, sleeping, idle,
    //    not on the exempt list, and old enough that "just detached and
    //    still in use" cannot explain them.
    let mut roots: Vec<i32> = Vec::new();
    for entry in table.entries.values() {
        if entry.pid <= 1 || entry.pid == self_pid {
            continue;
        }
        if !parent_comm_is_systemd(proc_root, entry.ppid) {
            continue;
        }
        if entry.tty_nr != 0 {
            continue;
        }
        if entry.state != 'S' {
            continue;
        }
        if matches_exempt(&entry.exe, &policy.exempt_names) {
            continue;
        }
        if entry.cpu_ticks / ticks_per_sec.max(1) > policy.max_cpu_seconds {
            continue;
        }
        // Age: a tree that has only just started cannot be a leak that
        // anybody has had a chance to notice, and a freshly detached
        // browser may still have a client attached to it.
        let age_secs = crate::reap::elapsed_secs(entry.starttime, ticks_per_sec, boot_time, now);
        if age_secs < FORGOTTEN_MIN_TREE_AGE_SECS {
            continue;
        }
        roots.push(entry.pid);
    }
    roots.sort_unstable();

    // 2. Weigh each root's tree.
    let mut claimed: HashSet<i32> = HashSet::new();
    for root_pid in roots {
        // A member of an already-weighed tree is not a tree of its own.
        if claimed.contains(&root_pid) {
            continue;
        }
        let Some(root) = table.get(root_pid) else { continue };
        let verdict = classify_desktop_session(&root.args);
        let why = match verdict {
            DesktopVerdict::Test { why } => why,
            DesktopVerdict::Desktop { reason } => {
                let tree = build_tree(proc_root, &table, root_pid, policy, boot_time, now, ticks_per_sec);
                claimed.extend(tree.members.iter().map(|m| m.pid));
                // A veto is only worth reporting when the tree is actually
                // heavy: the operator wants to see that a heavy Chrome was
                // weighed and left alone, not a list of every helper.
                if tree.aggregate_footprint_mb >= per_process_floor_mb {
                    out.vetoed.push(VetoedTree {
                        root_pid: tree.root_pid,
                        root_comm: tree.root_comm.clone(),
                        aggregate_footprint_mb: tree.aggregate_footprint_mb,
                        member_count: tree.member_count,
                        reason,
                    });
                }
                continue;
            }
            DesktopVerdict::NotABrowser => continue,
        };

        let tree = build_tree(proc_root, &table, root_pid, policy, boot_time, now, ticks_per_sec);
        claimed.extend(tree.members.iter().map(|m| m.pid));
        if tree.aggregate_footprint_mb >= tree_floor_mb
            || tree.root_footprint_mb >= per_process_floor_mb
        {
            out.candidates.push(ForgottenTree { why, ..tree });
        }
    }

    out.candidates
        .sort_by_key(|c| std::cmp::Reverse(c.aggregate_footprint_mb));
    out.vetoed
        .sort_by_key(|c| std::cmp::Reverse(c.aggregate_footprint_mb));
    out
}
