use super::*;
use std::cell::RefCell;

fn guard() -> GuardPolicy {
    GuardPolicy {
        service_memory_growth_window_secs: 60,
        service_memory_growth_min_mb: 10,
        service_memory_growth_targets: vec!["system:probe.service".into()],
        ..GuardPolicy::default()
    }
}
fn observation(resident_mb: u64, swap_mb: Option<u64>) -> Observation {
    let mut row = Observation::unavailable("system:probe.service", "missing counter");
    row.generation = Some(Generation {
        boot: "01234567-0123-4567-89ab-0123456789ab".into(),
        target: Target::parse("system:probe.service").unwrap(),
        user_uid: None,
        invocation: "a".repeat(32),
        group: "/system.slice/probe.service".into(),
        device: 1,
        inode: 2,
        pid: 7,
        identity: ProcessIdentity {
            comm: "probe".into(),
            starttime: 100,
        },
    });
    row.resident_bytes = Some(resident_mb * MIB);
    row.swap_bytes = swap_mb.map(|mb| mb * MIB);
    row.footprint_bytes = row
        .resident_bytes
        .zip(row.swap_bytes)
        .and_then(|(a, b)| a.checked_add(b));
    row.reason = row
        .footprint_bytes
        .is_none()
        .then(|| "swap accounting unavailable".into());
    row
}
fn at(
    tracker: &mut Tracker,
    guard: &GuardPolicy,
    start: Instant,
    secs: u64,
    row: Observation,
) -> Report {
    tracker.observe(guard, vec![row], start + Duration::from_secs(secs))
}

#[test]
fn sustained_growth_exposes_counter_baseline_elapsed_window_and_rate() {
    let mut tracker = Tracker::default();
    let start = Instant::now();
    let g = guard();
    for secs in [0, 15, 30, 45, 60] {
        let report = at(
            &mut tracker,
            &g,
            start,
            secs,
            observation(100 + secs, Some(20)),
        );
        let row = &report.observations[0];
        if secs < 60 {
            assert_eq!(row.status, "warming_up");
        } else {
            assert_eq!(row.status, "sustained_growth");
            assert_eq!(row.resident_bytes, Some(160 * MIB));
            assert_eq!(row.swap_bytes, Some(20 * MIB));
            assert_eq!(row.baseline_resident_bytes, Some(100 * MIB));
            assert_eq!(row.baseline_swap_bytes, Some(20 * MIB));
            assert_eq!(row.elapsed_secs, Some(60.0));
            assert_eq!(row.growth_bytes, Some(60 * MIB));
            assert!((row.growth_mib_per_min.unwrap() - 60.0).abs() < 0.001);
            let json = serde_json::to_value(&report).unwrap();
            assert_eq!(json["observations"][0]["footprint_bytes"], 180 * MIB);
            assert!(report.summary().contains(&row.target));
            assert!(report.summary().contains("60.0s"));
            assert!(report.summary().contains("60.00MiB/min"));
        }
    }
}

#[test]
fn a_single_burst_plateau_or_resident_swap_migration_is_not_sustained_growth() {
    let g = guard();
    let start = Instant::now();
    for mode in ["early_burst", "late_burst", "noisy_early_burst", "noisy_late_burst", "migration", "flat"] {
        let mut tracker = Tracker::default();
        for secs in (0..=180).step_by(15) {
            let row = match mode {
                "early_burst" => observation(if secs == 0 { 100 } else { 500 }, Some(0)),
                "noisy_early_burst" => observation(if secs == 0 { 100 } else { 500 + secs / 60 }, Some(0)),
                "noisy_late_burst" => observation(if secs < 60 { 100 + secs / 30 } else { 500 + secs / 60 }, Some(0)),
                "late_burst" => observation(if secs < 60 { 100 } else { 500 }, Some(0)),
                "migration" => observation(500 - secs, Some(secs)),
                _ => observation(500, Some(0)),
            };
            let report = at(&mut tracker, &g, start, secs, row);
            assert_ne!(
                report.observations[0].status, "sustained_growth",
                "{mode} {secs}"
            );
        }
    }
}

#[test]
fn swap_only_growth_and_irregular_intervals_use_elapsed_time_not_poll_count() {
    let g = guard();
    let start = Instant::now();
    let mut tracker = Tracker::default();
    for secs in [0, 11, 27, 42, 61] {
        let report = at(&mut tracker, &g, start, secs, observation(100, Some(secs)));
        if secs == 61 {
            let row = &report.observations[0];
            assert_eq!(row.status, "sustained_growth");
            assert_eq!(row.elapsed_secs, Some(61.0));
            assert!((row.growth_mib_per_min.unwrap() - 60.0).abs() < 0.001);
        }
    }
}

#[test]
fn service_and_process_generation_changes_never_bridge_history() {
    let g = guard();
    let start = Instant::now();
    for changed in [
        "boot",
        "invocation",
        "cgroup_inode",
        "pid_reuse",
        "main_pid",
        "exec",
    ] {
        let mut tracker = Tracker::default();
        at(&mut tracker, &g, start, 0, observation(100, Some(0)));
        at(&mut tracker, &g, start, 15, observation(200, Some(0)));
        let mut next = observation(900, Some(0));
        let generation = next.generation.as_mut().unwrap();
        match changed {
            "boot" => generation.boot = "new boot".into(),
            "invocation" => generation.invocation = "b".repeat(32),
            "cgroup_inode" => generation.inode += 1,
            "pid_reuse" => generation.identity.starttime += 1,
            "main_pid" => generation.pid += 1,
            "exec" => generation.identity.comm = "other".into(),
            _ => unreachable!(),
        }
        let report = at(&mut tracker, &g, start, 30, next);
        let row = &report.observations[0];
        assert_eq!(row.status, "reset", "{changed}");
        assert_eq!(row.elapsed_secs, Some(0.0));
        assert_eq!(row.growth_bytes, Some(0));
        assert!(row.growth_mib_per_min.is_none());
    }
}

#[test]
fn release_missing_data_disappearance_clock_and_sampling_gaps_reset_comparison() {
    let g = guard();
    let start = Instant::now();
    for mode in [
        "drop",
        "missing_swap",
        "missing_identity",
        "disappear",
        "duplicate_clock",
        "backward_clock",
        "gap",
    ] {
        let mut tracker = Tracker::default();
        at(&mut tracker, &g, start, 0, observation(100, Some(0)));
        at(&mut tracker, &g, start, 15, observation(200, Some(0)));
        match mode {
            "missing_swap" => {
                let report = at(&mut tracker, &g, start, 30, observation(900, None));
                let row = &report.observations[0];
                assert_eq!(row.resident_bytes, Some(900 * MIB));
                assert!(row.swap_bytes.is_none());
                assert!(row.footprint_bytes.is_none());
                assert!(row.growth_bytes.is_none());
                assert!(serde_json::to_value(row).unwrap()["swap_bytes"].is_null());
                let report = at(&mut tracker, &g, start, 45, observation(1000, Some(0)));
                assert_eq!(report.observations[0].elapsed_secs, Some(0.0));
            }
            "missing_identity" => {
                let mut row = observation(900, Some(0));
                row.generation = None;
                at(&mut tracker, &g, start, 30, row);
                assert!(tracker.histories.is_empty());
            }
            "disappear" => {
                tracker.observe(&g, vec![], start + Duration::from_secs(30));
                assert!(tracker.histories.is_empty());
            }
            _ => {
                let (secs, total) = match mode {
                    "drop" => (30, 50),
                    "duplicate_clock" => (15, 900),
                    "backward_clock" => (10, 900),
                    _ => (46, 900),
                };
                let report = at(&mut tracker, &g, start, secs, observation(total, Some(0)));
                assert_eq!(report.observations[0].status, "reset", "{mode}");
                assert_eq!(report.observations[0].elapsed_secs, Some(0.0));
            }
        }
    }
}

#[test]
fn history_and_event_signatures_stay_bounded_and_quiet() {
    let mut g = guard();
    g.service_memory_growth_min_mb = 1000;
    let start = Instant::now();
    let mut tracker = Tracker::default();
    for secs in 0..=1000 {
        let rows = (0..MAX_TARGETS + 5)
            .map(|id| {
                let mut row = observation(100 + secs, Some(0));
                row.target = format!("system:probe{id}.service");
                row
            })
            .collect();
        let report = tracker.observe(&g, rows, start + Duration::from_secs(secs));
        assert_eq!(report.observations.len(), MAX_TARGETS);
        assert!(tracker.histories.len() <= MAX_TARGETS);
        assert!(tracker
            .histories
            .values()
            .all(|h| h.points.len() <= MAX_HISTORY));
    }
    let mut tracker = Tracker::default();
    let warming = at(&mut tracker, &g, start, 0, observation(100, Some(0)));
    let reset = at(&mut tracker, &g, start, 15, observation(50, Some(0)));
    assert_eq!(warming.signature(), reset.signature());
    g.monitor_service_memory_growth = false;
    let report = at(&mut tracker, &g, start, 30, observation(900, Some(0)));
    assert!(!report.enabled);
    assert!(report.observations.is_empty());
    assert!(tracker.histories.is_empty());
}

struct Fixture {
    _root: tempfile::TempDir,
    roots: Roots,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let proc = root.path().join("proc");
        let cgroup = root.path().join("cgroup");
        fs::create_dir_all(proc.join("sys/kernel/random")).unwrap();
        fs::write(
            proc.join("sys/kernel/random/boot_id"),
            "01234567-0123-4567-89ab-0123456789ab\n",
        )
        .unwrap();
        fs::create_dir_all(cgroup.join("system.slice/probe.service")).unwrap();
        let result = Self {
            _root: root,
            roots: Roots {
                proc,
                cgroup,
                verify_host: false,
            },
        };
        result.process(100);
        result.counters("104857600", Some("20971520"));
        result
    }
    fn process(&self, start: u64) {
        let dir = self.roots.proc.join("7");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("comm"), "probe\n").unwrap();
        let mut fields = vec!["S".to_string()];
        fields.extend(std::iter::repeat_n("0".to_string(), 18));
        fields.push(start.to_string());
        fs::write(
            dir.join("stat"),
            format!("7 (probe) {}\n", fields.join(" ")),
        )
        .unwrap();
        fs::write(dir.join("cgroup"), "0::/system.slice/probe.service\n").unwrap();
    }
    fn counters(&self, resident: &str, swap: Option<&str>) {
        let dir = self.roots.cgroup.join("system.slice/probe.service");
        fs::write(dir.join("memory.current"), resident).unwrap();
        if let Some(swap) = swap {
            fs::write(dir.join("memory.swap.current"), swap).unwrap();
        } else {
            let _ = fs::remove_file(dir.join("memory.swap.current"));
        }
    }
    fn properties(&self) -> String {
        format!("Id=probe.service\nLoadState=loaded\nActiveState=active\nInvocationID={}\nControlGroup=/system.slice/probe.service\nMainPID=7\n", "a".repeat(32))
    }
}
struct FakeProbe<'a> {
    text: String,
    calls: RefCell<Vec<(Manager, Vec<String>)>>,
    after: RefCell<Option<Box<dyn FnOnce() + 'a>>>,
    second: Option<String>,
}
impl<'a> FakeProbe<'a> {
    fn new(text: String) -> Self {
        Self {
            text,
            calls: RefCell::new(Vec::new()),
            after: RefCell::new(None),
            second: None,
        }
    }
}
impl Probe for FakeProbe<'_> {
    async fn properties(&self, manager: Manager, units: &[String]) -> Result<String, String> {
        self.calls.borrow_mut().push((manager, units.to_vec()));
        if self.calls.borrow().len() == 2 {
            if let Some(callback) = self.after.borrow_mut().take() {
                callback();
            }
            if let Some(second) = &self.second {
                return Ok(second.clone());
            }
        }
        Ok(self.text.clone())
    }
}

#[tokio::test]
async fn collector_reads_fixture_counters_and_only_queries_explicit_manager() {
    let fixture = Fixture::new();
    let probe = FakeProbe::new(fixture.properties());
    let g = guard();
    let before = fs::read(
        fixture
            .roots
            .cgroup
            .join("system.slice/probe.service/memory.current"),
    )
    .unwrap();
    let rows = collect(&probe, &fixture.roots, &g.service_memory_growth_targets).await;
    assert_eq!(rows[0].resident_bytes, Some(100 * MIB));
    assert_eq!(rows[0].swap_bytes, Some(20 * MIB));
    assert!(rows[0].generation.is_some());
    assert_eq!(
        *probe.calls.borrow(),
        vec![(Manager::System, vec!["probe.service".into()]); 2]
    );
    assert_eq!(
        before,
        fs::read(
            fixture
                .roots
                .cgroup
                .join("system.slice/probe.service/memory.current")
        )
        .unwrap()
    );
    let mut runtime = crate::GuardRuntimeState::default();
    runtime.oom_biased_pids.insert(99, (0, ProcessIdentity { comm: "protected".into(), starttime: 55 }));
    let snapshot = runtime.oom_biased_pids.clone();
    let report = runtime
        .service_memory_growth
        .observe(&g, rows, Instant::now());
    assert_eq!(report.observations[0].status, "warming_up");
    assert_eq!(runtime.oom_biased_pids, snapshot);
    assert!(runtime.capped_pids.is_empty());
    assert!(runtime.memory_reniced_pids.is_empty());
}

#[tokio::test]
async fn manager_and_process_races_or_cgroup_replacement_make_sample_unavailable() {
    for changed in ["invocation", "pid_reuse", "migration", "cgroup"] {
        let fixture = Fixture::new();
        let mut probe = FakeProbe::new(fixture.properties());
        match changed {
            "invocation" => {
                probe.second = Some(
                    fixture
                        .properties()
                        .replace(&"a".repeat(32), &"b".repeat(32)),
                )
            }
            "pid_reuse" => probe.after = RefCell::new(Some(Box::new(|| fixture.process(101)))),
            "migration" => probe.after = RefCell::new(Some(Box::new(|| fs::write(fixture.roots.proc.join("7/cgroup"), "0::/system.slice/unrelated.service\n").unwrap()))),
            _ => {
                probe.after = RefCell::new(Some(Box::new(|| {
                    let path = fixture.roots.cgroup.join("system.slice/probe.service");
                    fs::rename(
                        &path,
                        fixture.roots.cgroup.join("system.slice/previous.service"),
                    )
                    .unwrap();
                    fs::create_dir(&path).unwrap();
                    fixture.counters("104857600", Some("20971520"));
                })))
            }
        }
        let rows = collect(
            &probe,
            &fixture.roots,
            &guard().service_memory_growth_targets,
        )
        .await;
        assert!(rows[0].generation.is_none(), "{changed}");
        assert!(rows[0].footprint_bytes.is_none());
    }
}

#[tokio::test]
async fn unsafe_paths_symlinks_missing_counters_and_unknown_managers_do_not_become_zero() {
    let fixture = Fixture::new();
    let g = guard();
    for path in [
        "/",
        "/system.slice/../probe.service",
        "/system.slice//probe.service",
        "/system.slice/./probe.service",
    ] {
        let probe = FakeProbe::new(
            fixture
                .properties()
                .replace("/system.slice/probe.service", path),
        );
        let rows = collect(&probe, &fixture.roots, &g.service_memory_growth_targets).await;
        assert!(rows[0].footprint_bytes.is_none(), "{path}");
    }
    fixture.counters("104857600", None);
    let probe = FakeProbe::new(fixture.properties());
    let rows = collect(&probe, &fixture.roots, &g.service_memory_growth_targets).await;
    assert_eq!(rows[0].resident_bytes, Some(100 * MIB));
    assert!(rows[0].swap_bytes.is_none());
    assert!(rows[0].footprint_bytes.is_none());
    let scalar = fixture
        .roots
        .cgroup
        .join("system.slice/probe.service/memory.swap.current");
    let external = fixture.roots.cgroup.join("outside");
    fs::write(&external, "0").unwrap();
    std::os::unix::fs::symlink(&external, &scalar).unwrap();
    let rows = collect(&probe, &fixture.roots, &g.service_memory_growth_targets).await;
    assert!(rows[0].swap_bytes.is_none());
    assert!(Target::parse("user:--bad.service").is_none());
    assert!(Target::parse("other:probe.service").is_none());
    let rows = collect(&probe, &fixture.roots, &["other:probe.service".into()]).await;
    assert!(rows[0].generation.is_none());
    assert!(parse_units(&format!(
        "{}\n{}",
        fixture.properties(),
        fixture.properties()
    ))
    .is_err());
    assert!(parse_units(&"x".repeat(MAX_OUTPUT + 1)).is_err());
}

#[test]
fn old_policies_defaults_new_knobs_and_normalization_are_backward_compatible() {
    let old: GuardPolicy = toml::from_str("").unwrap();
    let default = GuardPolicy::default();
    assert_eq!(
        old.service_memory_growth_targets,
        default.service_memory_growth_targets
    );
    assert_eq!(
        old.service_memory_growth_window_secs,
        default.service_memory_growth_window_secs
    );
    let mut explicit: GuardPolicy = toml::from_str("monitor_service_memory_growth = false\nservice_memory_growth_window_secs=0\nservice_memory_growth_min_mb=0\nservice_memory_growth_targets=['system:probe.service']").unwrap();
    assert!(!explicit.monitor_service_memory_growth);
    crate::policy::normalize_guard_policy(&mut explicit);
    assert_eq!(explicit.service_memory_growth_window_secs, 60);
    assert_eq!(explicit.service_memory_growth_min_mb, 1);
    let mut enormous = GuardPolicy {
        service_memory_growth_window_secs: u64::MAX,
        service_memory_growth_min_mb: u64::MAX,
        service_memory_growth_targets: (0..100)
            .map(|i| format!("system:probe{i}.service"))
            .collect(),
        ..default
    };
    crate::policy::normalize_guard_policy(&mut enormous);
    assert_eq!(enormous.service_memory_growth_window_secs, 86_400);
    assert_eq!(enormous.service_memory_growth_targets.len(), MAX_TARGETS);
    assert!(enormous
        .service_memory_growth_min_mb
        .checked_mul(MIB)
        .is_some());
    assert!(crate::policy::normalize_guard_policy(&mut enormous).is_empty());
}

#[test]
fn equivalent_target_reordering_preserves_observed_growth() {
    let g = guard(); let start = Instant::now();
    let mut a = Tracker::default(); let mut b = Tracker::default();
    for secs in [0, 15, 30, 45, 60] {
        let first = observation(100 + secs, Some(0));
        let mut second = observation(500 + secs * 2, Some(20));
        second.target = "system:other.service".into();
        let x = a.observe(&g, vec![first.clone(), second.clone()], start + Duration::from_secs(secs));
        let y = b.observe(&g, vec![second, first], start + Duration::from_secs(secs));
        let values = |r: Report| r.observations.into_iter().map(|o| (o.target, (o.status, o.elapsed_secs, o.growth_bytes))).collect::<BTreeMap<_,_>>();
        assert_eq!(values(x), values(y));
    }
}

#[test]
fn successful_reload_clears_history_but_deferred_reload_keeps_old_observation_state() {
    let mut g = guard(); g.enabled = true;
    let mut runtime = crate::GuardRuntimeState::default();
    runtime.service_memory_growth.observe(&g, vec![observation(100, Some(0))], Instant::now());
    let mut next = g.clone(); next.monitor_service_memory_growth = false;
    assert!(matches!(crate::apply_policy_reload(&mut g, &mut runtime, next.clone(), false), crate::ReloadOutcome::Deferred));
    assert!(g.monitor_service_memory_growth); assert!(!runtime.service_memory_growth.histories.is_empty());
    assert!(matches!(crate::apply_policy_reload(&mut g, &mut runtime, next, true), crate::ReloadOutcome::Applied));
    assert!(!g.monitor_service_memory_growth); assert!(runtime.service_memory_growth.histories.is_empty());
}

#[tokio::test]
async fn reader_and_manager_requests_are_bounded_and_query_only() {
    assert_eq!(capped_output(std::io::Cursor::new(vec![b'x'; MAX_OUTPUT]), MAX_OUTPUT).await.unwrap().len(), MAX_OUTPUT);
    assert!(capped_output(std::io::Cursor::new(vec![b'x'; MAX_OUTPUT + 1]), MAX_OUTPUT).await.is_err());
    for manager in [Manager::System, Manager::User] {
        let cmd = manager_command("not-executed", manager, &["probe.service".into()]);
        let args: Vec<_> = cmd.as_std().get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args[1], "show");
        assert_eq!(args[args.len()-2..], ["--", "probe.service"]);
        assert!(!args.iter().any(|a| ["kill", "restart", "start", "stop", "set-property"].contains(a)));
    }
    let fixture = Fixture::new(); let probe = FakeProbe::new(fixture.properties());
    let names: Vec<_> = (0..MAX_TARGETS + 10).map(|i| format!("{}:probe{i}.service", if i % 2 == 0 { "system" } else { "user" })).collect();
    let rows = collect(&probe, &fixture.roots, &names).await;
    assert_eq!(rows.len(), MAX_TARGETS); assert_eq!(probe.calls.borrow().len(), 4);
    assert!(probe.calls.borrow().iter().all(|(_, units)| units.len() <= MAX_TARGETS));
}

#[tokio::test]
async fn missing_namespace_or_boot_provenance_and_empty_targets_do_not_probe_managers() {
    let mut fixture = Fixture::new(); let probe = FakeProbe::new(fixture.properties());
    fixture.roots.verify_host = true;
    let rows = collect(&probe, &fixture.roots, &guard().service_memory_growth_targets).await;
    assert_eq!(rows[0].status, "unavailable"); assert!(probe.calls.borrow().is_empty());
    fs::remove_file(fixture.roots.proc.join("sys/kernel/random/boot_id")).unwrap();
    let rows = collect(&probe, &fixture.roots, &guard().service_memory_growth_targets).await;
    assert!(rows[0].generation.is_none()); assert!(probe.calls.borrow().is_empty());
    assert!(collect(&probe, &fixture.roots, &[]).await.is_empty());
}

#[tokio::test]
async fn malformed_overflow_and_directory_symlink_metrics_are_explicitly_unavailable() {
    let fixture = Fixture::new(); let probe = FakeProbe::new(fixture.properties()); let g = guard();
    for (resident, swap) in [("-1", "0"), ("bad", "0"), ("18446744073709551615", "1")] {
        fixture.counters(resident, Some(swap));
        let rows = collect(&probe, &fixture.roots, &g.service_memory_growth_targets).await;
        assert!(rows[0].footprint_bytes.is_none());
    }
    fixture.counters("0", Some("0"));
    let rows = collect(&probe, &fixture.roots, &g.service_memory_growth_targets).await;
    assert_eq!(rows[0].footprint_bytes, Some(0));
    let path = fixture.roots.cgroup.join("system.slice/probe.service");
    let moved = fixture.roots.cgroup.join("separate.service"); fs::rename(&path, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &path).unwrap();
    let rows = collect(&probe, &fixture.roots, &g.service_memory_growth_targets).await;
    assert!(rows[0].generation.is_none());
}

