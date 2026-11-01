//! Disposable proc fixtures protect swap-before-shortlist ordering, explicit
//! unknown measurements, incarnation safety and stable selection. No live
//! processes, signals, configuration or mitigation operations are involved.
use super::*;

fn row(pid: i32, rss_mb: u64, command: &str) -> ProcSample {
    parse_ps_output(&format!("{pid} 1 0 {} 0 {command}\n", rss_mb * 1024))
        .pop()
        .unwrap()
}

fn identity(root: &Path, pid: i32, command: &str, starttime: u64) {
    let dir = root.join(pid.to_string());
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("comm"), format!("{command}\n")).unwrap();
    let mut fields = vec!["S".to_string()];
    fields.extend(std::iter::repeat_n("0".to_string(), 18));
    fields.push(starttime.to_string());
    fs::write(
        dir.join("stat"),
        format!("{pid} ({command}) {}\n", fields.join(" ")),
    )
    .unwrap();
}

fn swap(root: &Path, pid: i32, mb: u64) {
    fs::write(
        root.join(pid.to_string()).join("status"),
        format!("Name:\tworker\nVmSwap:\t{} kB\n", mb * 1024),
    )
    .unwrap();
}

fn sample(root: &Path, parsed: Vec<ProcSample>) -> Vec<ProcSample> {
    verified_process_samples_with(root, parsed, |pid| read_proc_swap_mb_from(root, pid))
}

fn pids(samples: &[ProcSample]) -> Vec<i32> {
    samples.iter().map(|p| p.pid).collect()
}

#[test]
fn swapped_offender_outside_rss_shortlist_is_selected() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let mut parsed = Vec::new();
    for pid in 1..=8 {
        let rss = if pid == 8 { 1 } else { 4096 + pid as u64 };
        parsed.push(row(pid, rss, "worker"));
        identity(root, pid, "worker", 100 + pid as u64);
        swap(root, pid, if pid == 8 { 32 * 1024 } else { 0 });
    }
    assert!(parsed.iter().all(|p| p.swap_mb.is_none()));
    let mut reads = Vec::new();
    let samples = verified_process_samples_with(root, parsed.clone(), |pid| {
        reads.push(pid);
        read_proc_swap_mb_from(root, pid)
    });
    // One bounded swap read per verified row, not just the five largest RSS.
    assert_eq!(reads, pids(&parsed));
    let (top, unavailable) = memory_pressure_offenders(&samples, &HashSet::new());
    assert_eq!(top.len(), MEMORY_OFFENDER_LIMIT);
    assert_eq!(top[0].pid, 8);
    assert_eq!(top[0].swap_mb, Some(32 * 1024));
    assert_eq!(unavailable, 0);
    assert!(samples.iter().all(|p| p.starttime == 100 + p.pid as u64));

    // Discriminating old-algorithm control on disposable data: enriching only
    // the RSS shortlist cannot recover this offender, even with a final sort.
    parsed.sort_by_key(|p| std::cmp::Reverse(p.rss_mb));
    parsed.truncate(MEMORY_OFFENDER_LIMIT);
    let (old_top, _) = memory_pressure_offenders(&sample(root, parsed), &HashSet::new());
    assert!(!pids(&old_top).contains(&8));
}

#[test]
fn swapping_and_equal_footprints_are_independent_of_input_order() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let mut parsed = Vec::new();
    for pid in 1..=9 {
        identity(root, pid, "worker", pid as u64);
        parsed.push(row(pid, if pid <= 3 { 1 } else { 101 }, "worker"));
        swap(root, pid, if pid <= 3 { 100 } else { 0 });
    }
    let expected = vec![1, 2, 3, 4, 5];
    for shift in 0..parsed.len() {
        let mut reordered = parsed.clone();
        reordered.rotate_left(shift);
        for reversed in [false, true] {
            if reversed {
                reordered.reverse();
            }
            let samples = sample(root, reordered.clone());
            let (top, _) = memory_pressure_offenders(&samples, &HashSet::new());
            assert_eq!(pids(&top), expected);
        }
    }
}

#[test]
fn unknown_swap_is_not_a_measured_zero_even_outside_shortlist() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let mut parsed = Vec::new();
    for pid in 1..=8 {
        identity(root, pid, "worker", pid as u64);
        parsed.push(row(pid, if pid == 8 { 1 } else { 1024 }, "worker"));
        swap(root, pid, 0);
    }
    // A directory makes the read fail under root too; chmod-only fixtures
    // would accidentally remain readable in privileged test environments.
    let status = root.join("8/status");
    fs::remove_file(&status).unwrap();
    fs::create_dir(&status).unwrap();
    let samples = sample(root, parsed);
    let unknown = samples.iter().find(|p| p.pid == 8).unwrap();
    assert!(unknown.swap_mb.is_none());
    assert!(serde_json::to_value(unknown).unwrap()["swap_mb"].is_null());
    assert_eq!(memory_pressure_footprint_mb(unknown), unknown.rss_mb);
    let measured = samples.iter().find(|p| p.pid == 1).unwrap();
    assert_eq!(measured.swap_mb, Some(0));
    assert_eq!(serde_json::to_value(measured).unwrap()["swap_mb"], 0);
    let mut same_process_measured_zero = unknown.clone();
    same_process_measured_zero.swap_mb = Some(0);
    assert_ne!(
        memory_offender_description(unknown),
        memory_offender_description(&same_process_measured_zero)
    );
    let (top, unavailable) = memory_pressure_offenders(&samples, &HashSet::new());
    assert!(!pids(&top).contains(&8));
    // Even an omitted unknown is disclosed as incomplete ranking coverage.
    assert_eq!(unavailable, 1);
}

#[test]
fn swap_parser_rejects_missing_malformed_and_oversized_status() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    identity(root, 7, "worker", 100);
    let status = root.join("7/status");
    assert_eq!(read_proc_swap_mb_from(root, 7), None);
    for text in [
        "Name: worker\n",
        "VmSwap:\n",
        "VmSwap: -1 kB\n",
        "VmSwap: 18446744073709551616 kB\n",
        "VmSwap: broken kB\n",
        "VmSwap: 1024 MB\n",
        "VmSwap: 1024\n",
        "VmSwap: 1024 kB extra\n",
        "VmSwap: 1024 kB\nVmSwap: 2048 kB\n",
    ] {
        fs::write(&status, text).unwrap();
        assert_eq!(read_proc_swap_mb_from(root, 7), None, "{text:?}");
    }
    for (text, expected) in [("VmSwap: 0 kB\n", 0), ("VmSwap:\t8192 kB\n", 8)] {
        fs::write(&status, text).unwrap();
        assert_eq!(read_proc_swap_mb_from(root, 7), Some(expected));
    }
    let mut bounded = "VmSwap: 8192 kB\n".to_string();
    bounded.push_str(&"x".repeat(MAX_PROCESS_STATUS_BYTES - bounded.len()));
    fs::write(&status, &bounded).unwrap();
    assert_eq!(read_proc_swap_mb_from(root, 7), Some(8));
    bounded.push('x');
    fs::write(&status, bounded).unwrap();
    assert_eq!(read_proc_swap_mb_from(root, 7), None);
    fs::write(&status, [0xff, 0xfe]).unwrap();
    assert_eq!(read_proc_swap_mb_from(root, 7), None);
}

#[test]
fn process_churn_across_swap_read_discards_the_sample() {
    for change in ["reuse", "exec", "exit", "unreadable_identity"] {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        identity(root, 7, "worker", 100);
        swap(root, 7, 32 * 1024);
        let samples = verified_process_samples_with(root, vec![row(7, 1, "worker")], |pid| {
            let reading = read_proc_swap_mb_from(root, pid);
            match change {
                "reuse" => identity(root, pid, "worker", 101),
                "exec" => identity(root, pid, "different", 100),
                "exit" => fs::remove_dir_all(root.join(pid.to_string())).unwrap(),
                "unreadable_identity" => fs::remove_file(root.join("7/stat")).unwrap(),
                _ => unreachable!(),
            }
            reading
        });
        assert!(samples.is_empty(), "{change}");
    }
}

#[test]
fn initial_identity_failure_does_not_read_swap_or_fabricate_samples() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    identity(root, 1, "different", 100);
    identity(root, 2, "worker", 100);
    fs::write(root.join("2/stat"), "malformed").unwrap();
    let samples = verified_process_samples_with(
        root,
        vec![
            row(1, 1, "worker"),
            row(2, 1, "worker"),
            row(3, 1, "worker"),
        ],
        |_| panic!("unverified PID must not be sampled"),
    );
    assert!(samples.is_empty());
}

#[test]
fn exemptions_and_kernel_threads_never_become_memory_offenders() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let mut parsed = Vec::new();
    for (pid, command, mb) in [
        (1, "editor", 64 * 1024),
        (2, "kworker/0:1", 64 * 1024),
        (3, "worker", 1),
    ] {
        identity(root, pid, command, pid as u64);
        swap(root, pid, mb);
        parsed.push(row(pid, 1, command));
    }
    let samples = sample(root, parsed);
    let (top, unavailable) = memory_pressure_offenders(&samples, &HashSet::from(["editor".into()]));
    assert_eq!(pids(&top), vec![3]);
    assert_eq!(unavailable, 0);
    // An exempt process's unknown footprint cannot claim eligible coverage loss.
    let mut unknown_exempt = samples;
    unknown_exempt[0].swap_mb = None;
    let (_, unavailable) =
        memory_pressure_offenders(&unknown_exempt, &HashSet::from(["editor".into()]));
    assert_eq!(unavailable, 0);
}

#[test]
fn empty_selection_and_footprint_overflow_are_safe() {
    let (top, unavailable) = memory_pressure_offenders(&[], &HashSet::new());
    assert!(top.is_empty());
    assert_eq!(unavailable, 0);
    let mut large = row(1, 1, "worker");
    large.swap_mb = Some(u64::MAX);
    assert_eq!(memory_pressure_footprint_mb(&large), u64::MAX);
    let (top, _) = memory_pressure_offenders(&[large, row(2, 2, "worker")], &HashSet::new());
    assert_eq!(top[0].pid, 1);
}
