use super::*;
use std::{
    cell::RefCell,
    os::unix::{fs::symlink, net::UnixListener},
};

struct Fixture {
    temp: tempfile::TempDir,
    roots: Roots,
    now: SystemTime,
    listeners: Vec<UnixListener>,
    replies: HashMap<PathBuf, Reply>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let roots = Roots {
            proc: temp.path().join("proc"),
            locks: temp.path().join("locks"),
            producer: temp.path().join("tmux-persist-daemon"),
            uid: unsafe { libc::geteuid() },
        };
        fs::create_dir_all(&roots.proc).unwrap();
        fs::create_dir_all(&roots.locks).unwrap();
        fs::write(roots.proc.join("uptime"), "3600.0 0.0\n").unwrap();
        Self {
            temp,
            roots,
            now: SystemTime::now(),
            listeners: Vec::new(),
            replies: HashMap::new(),
        }
    }
    fn process(&self, pid: i32, start: u64, state: char) {
        let dir = self.roots.proc.join(pid.to_string());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("comm"), "fixture\n").unwrap();
        let mut fields = vec![state.to_string()];
        fields.extend(std::iter::repeat_n("0".into(), 18));
        fields.push(start.to_string());
        fs::write(
            dir.join("stat"),
            format!("{pid} (fixture) {}\n", fields.join(" ")),
        )
        .unwrap();
    }
    fn add(&mut self, index: i32, age: u64, state: char) -> PathBuf {
        let socket = self.temp.path().join(format!("socket{index}"));
        self.listeners.push(UnixListener::bind(&socket).unwrap());
        let pid = 100 + index;
        let producer = 200 + index;
        self.process(pid, 100, 'S');
        self.process(producer, 200, state);
        let dir = self.temp.path().join(format!("save{index}"));
        fs::create_dir(&dir).unwrap();
        fs::write(
            dir.join("snapshot"),
            "pane\tfixture\tcontent\nstate\tfixture\n",
        )
        .unwrap();
        File::options()
            .write(true)
            .open(dir.join("snapshot"))
            .unwrap()
            .set_modified(self.now - Duration::from_secs(age))
            .unwrap();
        symlink("snapshot", dir.join("last")).unwrap();
        let proc = self.roots.proc.join(pid.to_string());
        fs::create_dir_all(proc.join("net")).unwrap();
        fs::create_dir_all(proc.join("fd")).unwrap();
        fs::write(proc.join("net/unix"), format!("Num RefCount Protocol Flags Type St Inode Path\n000: 00000002 00000000 00010000 0001 01 {pid} {}\n", socket.display())).unwrap();
        symlink(format!("socket:[{pid}]"), proc.join("fd/7")).unwrap();
        let reply = Reply {
            pid,
            socket: socket.clone(),
            directory: dir.display().to_string(),
            marker: String::new(),
        };
        self.registration(&reply, producer);
        self.replies.insert(socket.clone(), reply);
        socket
    }
    fn registration(&self, reply: &Reply, pid: i32) {
        let lock = self.roots.locks.join(format!(
            "tmux-persist-{}-{}.lock",
            self.roots.uid,
            socket_key(reply.socket.as_os_str().as_bytes())
        ));
        fs::create_dir_all(&lock).unwrap();
        fs::write(lock.join("pid"), pid.to_string()).unwrap();
        fs::write(
            self.roots.proc.join(format!("{pid}/cmdline")),
            format!("bash\0{}\0", self.roots.producer.display()),
        )
        .unwrap();
        fs::write(
            self.roots.proc.join(format!("{pid}/environ")),
            format!(
                "SAFE=irrelevant\0TMUX={},{},0\0",
                reply.socket.display(),
                reply.pid
            ),
        )
        .unwrap();
    }
    fn probe(&self) -> Fake<'_> {
        Fake {
            replies: self.replies.clone(),
            calls: RefCell::new(Vec::new()),
            after: RefCell::new(None),
            delay_first: None,
        }
    }
}
type Callback<'a> = RefCell<Option<Box<dyn FnOnce() + 'a>>>;
struct Fake<'a> {
    replies: HashMap<PathBuf, Reply>,
    calls: RefCell<Vec<PathBuf>>,
    after: Callback<'a>,
    delay_first: Option<Duration>,
}
impl Probe for Fake<'_> {
    async fn query(&self, socket: &Path) -> Result<Reply, String> {
        self.calls.borrow_mut().push(socket.into());
        let count = self.calls.borrow().len();
        if count == 1 { if let Some(delay) = self.delay_first { tokio::time::sleep(delay).await; } }
        if count == 2 {
            if let Some(f) = self.after.borrow_mut().take() {
                f();
            }
        }
        self.replies
            .get(socket)
            .cloned()
            .ok_or("fixture server unavailable".into())
    }
}

// Process state is a separate observation: neither S nor even a fresh file proves autosave execution.
#[tokio::test]
async fn fresh_stale_and_stopped_are_distinct_facts_not_recovery_predicates() {
    let mut f = Fixture::new();
    let a = f.add(1, 10, 'S');
    let b = f.add(2, 1200, 'S');
    let c = f.add(3, 10, 'T');
    let probe = f.probe();
    let report = collect(&probe, &f.roots, &[a, b, c], f.now, 900).await;
    assert_eq!(report.servers[0].save.state, SaveState::Fresh);
    assert_eq!(report.servers[0].producer.state, ProducerState::Existing);
    assert_eq!(report.servers[1].save.state, SaveState::Stale);
    assert_eq!(report.servers[1].producer.state, ProducerState::Existing);
    assert_eq!(report.servers[2].producer.state, ProducerState::Stopped);
    assert_eq!(report.servers[2].save.state, SaveState::Fresh);
    let json = serde_json::to_value(&report).unwrap();
    assert!(json["servers"][0]["save"]["age_secs"]
        .as_u64()
        .is_some_and(|n| n >= 10));
    assert!(json["servers"][0]["server_starttime"].is_number());
    assert_eq!(json["servers"][2]["producer"]["process_state"], "T");
    assert!(report.servers[1].summary().contains("Stale"));
}

// Harmless environment/target order changes must not matter; a server-binding mismatch must.
#[tokio::test]
async fn multiple_servers_do_not_borrow_other_producers_or_evidence() {
    let mut f = Fixture::new();
    let a = f.add(1, 10, 'S');
    let b = f.add(2, 1200, 'S');
    fs::write(
        f.roots.proc.join("201/environ"),
        format!("TMUX={},{},0\0UNRELATED=changed\0", a.display(), 101),
    )
    .unwrap();
    let good = collect(&f.probe(), &f.roots, &[b.clone(), a.clone()], f.now, 900).await;
    assert_eq!(good.servers[0].save.state, SaveState::Stale);
    assert_eq!(good.servers[1].save.state, SaveState::Fresh);
    assert_eq!(good.servers[1].producer.state, ProducerState::Existing);
    fs::write(
        f.roots.proc.join("201/environ"),
        format!("TMUX={},{},0\0", b.display(), 102),
    )
    .unwrap();
    let bad = collect(&f.probe(), &f.roots, &[a], f.now, 900).await;
    assert_eq!(bad.servers[0].producer.state, ProducerState::Unavailable);
    f.replies.get_mut(&b).unwrap().directory = f.temp.path().join("save1").display().to_string();
    let shared = collect(
        &f.probe(),
        &f.roots,
        &f.replies.keys().cloned().collect::<Vec<_>>(),
        f.now,
        900,
    )
    .await;
    assert!(shared
        .servers
        .iter()
        .all(|s| s.save.state == SaveState::Shared));
    f.replies.get_mut(&b).unwrap().directory = f.temp.path().join("save2").display().to_string();
    fs::remove_file(f.temp.path().join("save2/snapshot")).unwrap();
    fs::hard_link(f.temp.path().join("save1/snapshot"), f.temp.path().join("save2/snapshot")).unwrap();
    // One server rejects the file as predating its own incarnation. Physical
    // identity must still prevent the other server claiming exclusive evidence.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
    f.process(102, 3595 * ticks, 'S');
    let linked = collect(&f.probe(), &f.roots, &f.replies.keys().cloned().collect::<Vec<_>>(), f.now, 900).await;
    assert!(linked.servers.iter().all(|s| s.save.state == SaveState::Shared));
}

#[tokio::test]
async fn missing_or_untrusted_provenance_is_unknown_not_healthy() {
    for mode in [
        "no_pid",
        "missing_process",
        "indeterminate_process",
        "reused_server_pid",
        "partial_record",
        "bad_command",
        "missing_env",
        "pid_reuse",
        "no_fd",
        "no_directory",
        "no_snapshot",
        "future",
        "previous_server",
        "bad_content",
        "unsafe_last",
    ] {
        let mut f = Fixture::new();
        let socket = f.add(1, 10, 'S');
        match mode {
            "no_pid" => {
                fs::remove_dir_all(&f.roots.locks).unwrap();
            }
            "missing_process" => {
                fs::remove_dir_all(f.roots.proc.join("201")).unwrap();
            }
            "indeterminate_process" => {
                fs::remove_dir_all(f.roots.proc.join("201")).unwrap();
                symlink("201", f.roots.proc.join("201")).unwrap();
            }
            "reused_server_pid" => f.process(101, 300, 'S'),
            "partial_record" => fs::write(f.temp.path().join("save1/snapshot"), "pane\tpartial").unwrap(),
            "bad_command" => {
                fs::write(
                    f.roots.proc.join("201/cmdline"),
                    format!("echo\0{}\0", f.roots.producer.display()),
                )
                .unwrap();
            }
            "missing_env" => {
                fs::remove_file(f.roots.proc.join("201/environ")).unwrap();
            }
            "pid_reuse" => {
                fs::write(f.roots.proc.join("201/cmdline"), "other\0").unwrap();
            }
            "no_fd" => {
                fs::remove_file(f.roots.proc.join("101/fd/7")).unwrap();
            }
            "no_directory" => {
                f.replies.get_mut(&socket).unwrap().directory.clear();
            }
            "no_snapshot" => {
                fs::remove_file(f.temp.path().join("save1/snapshot")).unwrap();
            }
            "future" => File::options()
                .write(true)
                .open(f.temp.path().join("save1/snapshot"))
                .unwrap()
                .set_modified(f.now + Duration::from_secs(100))
                .unwrap(),
            "previous_server" => File::options()
                .write(true)
                .open(f.temp.path().join("save1/snapshot"))
                .unwrap()
                .set_modified(f.now - Duration::from_secs(7200))
                .unwrap(),
            "bad_content" => {
                fs::write(f.temp.path().join("save1/snapshot"), "not a snapshot").unwrap()
            }
            "unsafe_last" => {
                fs::remove_file(f.temp.path().join("save1/last")).unwrap();
                symlink("../outside", f.temp.path().join("save1/last")).unwrap();
            }
            _ => unreachable!(),
        }
        let report = collect(&f.probe(), &f.roots, &[socket], f.now, 900).await;
        let row = &report.servers[0];
        if ["no_pid", "bad_command", "missing_env", "pid_reuse", "indeterminate_process", "reused_server_pid"].contains(&mode) {
            assert_eq!(row.producer.state, ProducerState::Unavailable, "{mode}");
        } else if mode == "missing_process" {
            assert_eq!(row.producer.state, ProducerState::Missing);
        } else {
            assert_eq!(row.save.state, SaveState::Unavailable, "{mode}");
        }
    }
}

#[tokio::test]
async fn query_bracket_rejects_server_and_producer_lifecycle_races() {
    for mode in ["server", "producer", "socket"] {
        let mut f = Fixture::new();
        let socket = f.add(1, 10, 'S');
        let probe = f.probe();
        probe.after.replace(Some(Box::new(|| match mode {
            "server" => f.process(101, 999, 'S'),
            "producer" => f.process(201, 999, 'T'),
            "socket" => {
                fs::remove_file(&socket).unwrap();
                fs::write(&socket, "not a socket").unwrap();
            }
            _ => unreachable!(),
        })));
        let report = collect(&probe, &f.roots, std::slice::from_ref(&socket), f.now, 900).await;
        if mode == "producer" {
            assert_eq!(report.servers[0].producer.state, ProducerState::Unavailable);
        } else {
            assert!(report.servers[0].server_pid.is_none());
        }
    }
}

// No real tmux/server/producer is launched or signalled; every endpoint and file is owned by TempDir.
#[tokio::test]
async fn inspection_is_query_only_and_preserves_owned_files_and_socket() {
    let mut f = Fixture::new();
    let socket = f.add(1, 10, 'S');
    let probe = f.probe();
    let files = [
        f.temp.path().join("save1/snapshot"),
        f.roots.proc.join("201/stat"),
        f.roots.proc.join("201/environ"),
    ];
    let before: Vec<_> = files.iter().map(|p| fs::read(p).unwrap()).collect();
    let socket_meta = fs::symlink_metadata(&socket).unwrap();
    let _ = collect(&probe, &f.roots, std::slice::from_ref(&socket), f.now, 900).await;
    assert_eq!(
        before,
        files
            .iter()
            .map(|p| fs::read(p).unwrap())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        socket_meta.ino(),
        fs::symlink_metadata(&socket).unwrap().ino()
    );
    assert_eq!(
        probe.calls.borrow().as_slice(),
        [socket.clone(), socket.clone()]
    );
    let cmd = command("not-executed", &socket);
    let args: Vec<_> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_str().unwrap())
        .collect();
    assert_eq!(args[0], "-S");
    assert_eq!(args[1], socket.to_str().unwrap());
    assert_eq!(&args[2..], ["display-message", "-p", FORMAT]);
}

// Earlier sockets may consume time; later fresh saves must not look future-dated.
#[tokio::test]
async fn delayed_first_server_does_not_shift_later_save_freshness() {
    let mut f = Fixture::new(); let a = f.add(1, 10, 'S'); let b = f.add(2, 10, 'S');
    let mut probe = f.probe(); probe.delay_first = Some(Duration::from_millis(1100));
    probe.after.replace(Some(Box::new(|| {
        File::options().write(true).open(f.temp.path().join("save2/snapshot")).unwrap()
            .set_modified(f.now + Duration::from_secs(1)).unwrap();
    })));
    let rows = collect(&probe, &f.roots, &[a, b], f.now, 900).await;
    assert_eq!(rows.servers[1].save.state, SaveState::Fresh);
    assert!(rows.servers[1].save.age_secs.is_some_and(|age| age < 60));
}

#[test]
fn cksum_protocol_and_policy_compatibility() {
    // POSIX cksum protocol bytes, not a directory inventory pin.
    assert_eq!(socket_key(b""), 4294967295);
    assert_eq!(socket_key(b"abc"), 1219131554);
    let old: GuardPolicy = toml::from_str("").unwrap();
    assert!(old.tmux_persistence_sockets.is_empty());
    assert_eq!(old.tmux_persistence_max_save_age_secs, default_max_age());
    let mut g: GuardPolicy = toml::from_str(
        "tmux_persistence_max_save_age_secs=0\ntmux_persistence_sockets=['/b','/a','/b']",
    )
    .unwrap();
    crate::policy::normalize_guard_policy(&mut g);
    assert_eq!(g.tmux_persistence_max_save_age_secs, 60);
    assert_eq!(g.tmux_persistence_sockets, ["/a", "/b"]);
    assert!(crate::policy::normalize_guard_policy(&mut g).is_empty());
}

#[tokio::test]
async fn budgets_and_parser_failures_are_explicit() {
    assert!(parse_reply("1\t/socket\t/dir\t\n").is_ok());
    assert!(parse_reply("0\t/socket\t/dir\t").is_err());
    assert!(parse_reply("1\t/socket\t/dir\t\textra").is_err());
    let mut f = Fixture::new();
    let a = f.add(1, 10, 'S');
    fs::write(
        f.roots.proc.join("101/net/unix"),
        vec![b'x'; SOCKET_TABLE + 1],
    )
    .unwrap();
    let report = collect(&f.probe(), &f.roots, &[a], f.now, 900).await;
    assert!(report.servers[0].server_pid.is_none());
    let sockets: Vec<_> = (0..MAX_SERVERS + 3)
        .map(|i| f.temp.path().join(format!("absent{i}")))
        .collect();
    assert_eq!(
        collect(&f.probe(), &f.roots, &sockets, f.now, 900)
            .await
            .servers
            .len(),
        MAX_SERVERS
    );
}
