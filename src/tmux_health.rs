//! Read-only, per-socket persistence observations for doctor. No recovery authority.
use crate::{policy::GuardPolicy, ProcessIdentity};
use serde::Serialize;
use std::{collections::HashMap, fs::{self, File, OpenOptions}, io::{Read, Error}, os::{fd::{AsRawFd, FromRawFd}, unix::{fs::{FileTypeExt, MetadataExt, OpenOptionsExt}, ffi::OsStrExt}}, path::{Component, Path, PathBuf}, process::Stdio, time::{Duration, SystemTime, UNIX_EPOCH}};

const MAX_SERVERS: usize = 8;
const SMALL: usize = 4096;
const RECORD: usize = 65536;
const SOCKET_TABLE: usize = 262144;
const MAX_FDS: usize = 128;
const FORMAT: &str = "#{pid}\t#{socket_path}\t#{@resurrect-dir}\t#{@continuum-save-last-timestamp}";
pub(crate) fn default_max_age() -> u64 { 900 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProducerState { Unavailable, Missing, Existing, Stopped }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SaveState { Unavailable, Fresh, Stale, Shared }
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Producer {
    pub(crate) state: ProducerState,
    pid: Option<i32>, starttime: Option<u64>, process_state: Option<char>,
    reason: String,
}
impl Producer {
    fn unknown(reason: &str) -> Self { Self { state: ProducerState::Unavailable, pid: None, starttime: None, process_state: None, reason: reason.into() } }
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Save {
    pub(crate) state: SaveState,
    directory: Option<PathBuf>, target: Option<String>, age_secs: Option<u64>,
    bytes: Option<u64>, device: Option<u64>, inode: Option<u64>,
    directory_identity: Option<(u64, u64)>,
    reason: String,
}
impl Save {
    fn unknown(reason: &str) -> Self { Self { state: SaveState::Unavailable, directory: None, target: None, age_secs: None, bytes: None, device: None, inode: None, directory_identity: None, reason: reason.into() } }
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Server {
    pub(crate) socket: PathBuf,
    pub(crate) server_pid: Option<i32>,
    server_starttime: Option<u64>,
    pub(crate) producer: Producer,
    pub(crate) save: Save,
    launch_marker_age_secs: Option<u64>,
    pub(crate) reason: String,
}
impl Server {
    fn unknown(socket: &Path, reason: &str) -> Self { Self { socket: socket.into(), server_pid: None, server_starttime: None, producer: Producer::unknown("server provenance unavailable"), save: Save::unknown("server provenance unavailable"), launch_marker_age_secs: None, reason: reason.into() } }
    pub(crate) fn summary(&self) -> String {
        format!("{}: server={}; producer={:?} ({}); save={:?}, age={}s ({}). {}", self.socket.display(), self.server_pid.map_or_else(|| "unavailable".into(), |p| p.to_string()), self.producer.state, self.producer.reason, self.save.state, self.save.age_secs.map_or_else(|| "unknown".into(), |a| a.to_string()), self.save.reason, self.reason)
    }
}
#[derive(Debug, Default, Serialize)]
pub(crate) struct Report {
    pub(crate) servers: Vec<Server>,
}
struct Roots { proc: PathBuf, locks: PathBuf, producer: PathBuf, uid: u32 }
impl Roots {
    fn host() -> Self { Self { proc: "/proc".into(), locks: "/tmp".into(), producer: dirs::home_dir().unwrap_or_default().join(".local/bin/tmux-persist-daemon"), uid: unsafe { libc::geteuid() } } }
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reply { pid: i32, socket: PathBuf, directory: String, marker: String }
fn parse_reply(text: &str) -> Result<Reply, String> {
    let fields: Vec<_> = text.trim_end_matches('\n').split('\t').collect();
    if fields.len() != 4 || fields.iter().any(|v| v.chars().any(char::is_control)) { return Err("invalid server reply".into()); }
    let pid = fields[0].parse::<i32>().ok().filter(|p| *p > 0).ok_or("server PID unavailable")?;
    Ok(Reply { pid, socket: fields[1].into(), directory: fields[2].into(), marker: fields[3].into() })
}
trait Probe { async fn query(&self, socket: &Path) -> Result<Reply, String>; }
struct Tmux;
fn command(bin: &str, socket: &Path) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(bin);
    c.arg("-S").arg(socket).args(["display-message", "-p", FORMAT]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    c
}
impl Probe for Tmux {
    async fn query(&self, socket: &Path) -> Result<Reply, String> {
        let bin = crate::resolve_bin_for_readonly_probe("tmux").map_err(|e| e.to_string())?;
        let mut child = command(&bin, socket).spawn().map_err(|e| e.to_string())?;
        let stdout = child.stdout.take().ok_or("stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("stderr unavailable")?;
        let (out, _, status) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::try_join!(crate::service_growth::capped_output(stdout, SMALL), crate::service_growth::capped_output(stderr, SMALL), child.wait())
        }).await.map_err(|_| "tmux query timeout")?.map_err(|e| e.to_string())?;
        if !status.success() { return Err("tmux query unavailable".into()); }
        parse_reply(std::str::from_utf8(&out).map_err(|_| "non-UTF8 server reply")?)
    }
}
fn safe_path(path: &Path) -> bool {
    path.is_absolute() && path.as_os_str().as_bytes().len() <= SMALL && !path.as_os_str().as_bytes().iter().any(|b| b.is_ascii_control()) && !path.components().any(|c| matches!(c, Component::ParentDir))
}
fn open_dir(path: &Path, uid: u32) -> Result<File, Error> {
    let f = OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
    if f.metadata()?.uid() != uid { return Err(Error::other("directory owner unavailable")); }
    Ok(f)
}
fn at(dir: &File, name: &str) -> Result<File, Error> {
    if name.is_empty() || Path::new(name).components().count() != 1 || !matches!(Path::new(name).components().next(), Some(Component::Normal(_))) { return Err(Error::other("unsafe file name")); }
    let name = std::ffi::CString::new(name).map_err(|_| Error::other("invalid file name"))?;
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK) };
    if fd < 0 { return Err(Error::last_os_error()); }
    let f = unsafe { File::from_raw_fd(fd) };
    if !f.metadata()?.is_file() { return Err(Error::other("not a regular file")); }
    Ok(f)
}
fn read(mut f: File, max: usize) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new(); (&mut f).take((max + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max { return Err(Error::other("read limit exceeded")); }
    Ok(bytes)
}
fn bytes(path: &Path, max: usize) -> Result<Vec<u8>, Error> {
    let f = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
    if !f.metadata()?.is_file() { return Err(Error::other("not a regular file")); }
    read(f, max)
}
fn text(path: &Path, max: usize) -> Result<String, Error> { String::from_utf8(bytes(path, max)?).map_err(|_| Error::other("invalid UTF8")) }
fn identity(roots: &Roots, pid: i32) -> Result<(ProcessIdentity, char), Error> {
    if fs::metadata(roots.proc.join(pid.to_string()))?.uid() != roots.uid { return Err(Error::other("process owner unavailable")); }
    let id = crate::read_proc_identity(&roots.proc, pid)?;
    let stat = text(&roots.proc.join(format!("{pid}/stat")), SMALL)?;
    let state = stat.rsplit_once(')').and_then(|(_, tail)| tail.split_whitespace().next()).and_then(|s| s.chars().next()).ok_or_else(|| Error::other("process state unavailable"))?;
    if crate::read_proc_identity(&roots.proc, pid)? != id { return Err(Error::other("process identity changed")); }
    Ok((id, state))
}
fn socket_key(input: &[u8]) -> u32 {
    // POSIX cksum, including length. Matches the existing producer's lock key.
    fn feed(mut crc: u32, byte: u8) -> u32 { crc ^= (byte as u32) << 24; for _ in 0..8 { crc = if crc & 0x80000000 != 0 { (crc << 1) ^ 0x04c11db7 } else { crc << 1 }; } crc }
    let mut crc = 0; for b in input { crc = feed(crc, *b); }
    let mut length = input.len(); while length != 0 { crc = feed(crc, length as u8); length >>= 8; }
    !crc
}
fn producer(roots: &Roots, server: &Reply) -> Producer {
    let lock = roots.locks.join(format!("tmux-persist-{}-{}.lock", roots.uid, socket_key(server.socket.as_os_str().as_bytes())));
    let result = (|| -> Result<Producer, Error> {
        let dir = open_dir(&lock, roots.uid)?;
        let pid = String::from_utf8(read(at(&dir, "pid")?, 32)?).ok().and_then(|p| p.trim().parse::<i32>().ok()).filter(|p| *p > 0).ok_or_else(|| Error::other("invalid registered PID"))?;
        if !roots.proc.join(pid.to_string()).exists() { return Ok(Producer { state: ProducerState::Missing, pid: Some(pid), starttime: None, process_state: None, reason: "registered PID absent; other producers not searched".into() }); }
        let (id, state) = identity(roots, pid)?;
        let cmd = bytes(&roots.proc.join(format!("{pid}/cmdline")), RECORD)?;
        if !cmd.split(|b| *b == 0).any(|a| a == roots.producer.as_os_str().as_bytes()) { return Err(Error::other("registered PID is not the recognized producer")); }
        let env = bytes(&roots.proc.join(format!("{pid}/environ")), RECORD)?;
        let bindings: Vec<_> = env.split(|b| *b == 0).filter_map(|v| v.strip_prefix(b"TMUX=")).collect();
        let expected = format!("{},{}", server.socket.display(), server.pid);
        if bindings.len() != 1 || !bindings[0].starts_with(format!("{expected},").as_bytes()) { return Err(Error::other("producer socket/server binding unavailable")); }
        let final_pid = read(at(&dir, "pid")?, 32)?;
        if String::from_utf8_lossy(&final_pid).trim() != pid.to_string() || identity(roots, pid)? != (id.clone(), state) { return Err(Error::other("producer registration/incarnation changed")); }
        Ok(Producer { state: if matches!(state, 'T' | 't') { ProducerState::Stopped } else if matches!(state, 'Z' | 'X' | 'x') { ProducerState::Missing } else { ProducerState::Existing }, pid: Some(pid), starttime: Some(id.starttime), process_state: Some(state), reason: "socket-bound registered producer; process state is not save success or a stop-cause diagnosis".into() })
    })();
    result.unwrap_or_else(|e| Producer::unknown(&e.to_string()))
}
fn listening_owner(roots: &Roots, server: &Reply) -> Result<(), Error> {
    let table = text(&roots.proc.join(format!("{}/net/unix", server.pid)), SOCKET_TABLE)?;
    let mut inodes = Vec::new();
    for line in table.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() == 8 && fields[3] == "00010000" && fields[4] == "0001" && fields[7].as_bytes() == server.socket.as_os_str().as_bytes() { inodes.push(format!("socket:[{}]", fields[6])); }
    }
    if inodes.is_empty() { return Err(Error::other("listening socket provenance unavailable (including unsupported whitespace paths)")); }
    for entry in fs::read_dir(roots.proc.join(format!("{}/fd", server.pid)))?.take(MAX_FDS) {
        if let Ok(link) = fs::read_link(entry?.path()) { if inodes.iter().any(|s| link.as_os_str() == std::ffi::OsStr::new(s)) { return Ok(()); } }
    }
    Err(Error::other("reported server PID is not an observable socket owner within fd budget"))
}
fn last_target(dir: &File) -> Result<String, Error> {
    let mut buf = vec![0u8; SMALL + 1];
    let n = unsafe { libc::readlinkat(dir.as_raw_fd(), c"last".as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 { // Some tools use a regular last file. No symlink-following fallback.
        at(dir, "last")?; return Ok("last".into());
    }
    if n as usize > SMALL { return Err(Error::other("snapshot link too long")); }
    buf.truncate(n as usize);
    let name = String::from_utf8(buf).map_err(|_| Error::other("non-UTF8 snapshot link"))?;
    at(dir, &name)?; Ok(name)
}
fn snapshot(roots: &Roots, server: &Reply, birth: SystemTime, now: SystemTime, max_age: u64) -> Save {
    let started = std::time::Instant::now();
    let mut observed = Save::unknown("save evidence unavailable");
    let result = (|| -> Result<(), Error> {
        if server.directory.is_empty() { return Err(Error::other("explicit @resurrect-dir unavailable; defaults not guessed")); }
        let path = crate::policy::expand_tilde(&server.directory);
        if !safe_path(&path) { return Err(Error::other("unsafe snapshot directory")); }
        let dir = open_dir(&path, roots.uid)?;
        let dir_meta = dir.metadata()?;
        observed.directory = Some(path.clone());
        observed.directory_identity = Some((dir_meta.dev(), dir_meta.ino()));
        let name = last_target(&dir)?; let file = at(&dir, &name)?; let before = file.metadata()?;
        if before.uid() != roots.uid || before.len() == 0 { return Err(Error::other("snapshot owner/content unavailable")); }
        let mut prefix = Vec::new(); file.take(RECORD as u64).read_to_end(&mut prefix)?;
        if !prefix.split(|b| *b == b'\n').any(|l| l.starts_with(b"pane\t")) { return Err(Error::other("pane record unavailable within inspection budget; restore not validated")); }
        let after = at(&dir, &name)?.metadata()?;
        if last_target(&dir)? != name || before.dev() != after.dev() || before.ino() != after.ino() || before.len() != after.len() || before.modified()? != after.modified()? { return Err(Error::other("snapshot changed during inspection")); }
        let modified = before.modified()?;
        let clock = now.checked_add(started.elapsed()).ok_or_else(|| Error::other("observation clock overflow"))?;
        let age = clock.duration_since(modified).map_err(|_| Error::other("future snapshot timestamp"))?.as_secs();
        if modified < birth { return Err(Error::other("snapshot predates current server incarnation")); }
        let current_dir = open_dir(&path, roots.uid)?.metadata()?;
        if (current_dir.dev(), current_dir.ino()) != (dir_meta.dev(), dir_meta.ino()) { return Err(Error::other("snapshot directory changed during inspection")); }
        observed.state = if age <= max_age { SaveState::Fresh } else { SaveState::Stale };
        observed.target = Some(name); observed.age_secs = Some(age); observed.bytes = Some(before.len()); observed.device = Some(before.dev()); observed.inode = Some(before.ino());
        observed.reason = "configured-path pane evidence only; not producer attribution, periodic execution or restore validation".into();
        Ok(())
    })(); if let Err(e) = result { observed.reason = e.to_string(); } observed
}
fn birth(roots: &Roots, id: &ProcessIdentity, now: SystemTime) -> Result<SystemTime, Error> {
    let uptime = text(&roots.proc.join("uptime"), 128)?.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()).filter(|n| n.is_finite() && *n >= 0.0).ok_or_else(|| Error::other("boot uptime unavailable"))?;
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks <= 0 { return Err(Error::other("clock tick rate unavailable")); }
    let age = uptime - id.starttime as f64 / ticks as f64;
    if !(0.0..=86400.0 * 365.0 * 100.0).contains(&age) { return Err(Error::other("server generation time unavailable")); }
    now.checked_sub(Duration::from_secs_f64(age)).ok_or_else(|| Error::other("server generation time overflow"))
}
async fn inspect(probe: &impl Probe, roots: &Roots, socket: &Path, now: SystemTime, max_age: u64) -> Result<Server, String> {
    let started = std::time::Instant::now();
    if !safe_path(socket) { return Err("unsafe socket path".into()); }
    let metadata = fs::symlink_metadata(socket).map_err(|_| "socket metadata unavailable")?;
    if !metadata.file_type().is_socket() || metadata.uid() != roots.uid { return Err("owned Unix socket unavailable".into()); }
    let before = probe.query(socket).await?;
    if before.socket != socket { return Err("server replied for another socket".into()); }
    let (id, state) = identity(roots, before.pid).map_err(|e| e.to_string())?;
    if matches!(state, 'T' | 't' | 'Z' | 'X' | 'x') { return Err("server process unavailable/stopped during observation; cause unknown".into()); }
    listening_owner(roots, &before).map_err(|e| e.to_string())?;
    let registered = producer(roots, &before);
    let now = now.checked_add(started.elapsed()).ok_or("observation clock overflow")?;
    let save = birth(roots, &id, now).map_or_else(|e| Save::unknown(&e.to_string()), |born| snapshot(roots, &before, born, now, max_age));
    let after = probe.query(socket).await?;
    let final_meta = fs::symlink_metadata(socket).map_err(|_| "socket disappeared")?;
    if before.pid != after.pid || before.socket != after.socket || before.directory != after.directory || identity(roots, before.pid).map_err(|e| e.to_string())? != (id.clone(), state) || metadata.dev() != final_meta.dev() || metadata.ino() != final_meta.ino() { return Err("server/socket/config incarnation changed during inspection".into()); }
    listening_owner(roots, &after).map_err(|e| e.to_string())?;
    let final_registered = producer(roots, &after);
    let registered = if registered == final_registered { registered } else { Producer::unknown("producer changed during server query bracket") };
    let marker = before.marker.parse::<u64>().ok().filter(|s| *s > 0).and_then(|s| UNIX_EPOCH.checked_add(Duration::from_secs(s))).and_then(|t| now.duration_since(t).ok()).map(|d| d.as_secs());
    Ok(Server { socket: socket.into(), server_pid: Some(before.pid), server_starttime: Some(id.starttime), producer: registered, save, launch_marker_age_secs: marker, reason: "observations only; continuum marker means launch, sleeping means existence; neither proves successful autosave".into() })
}
async fn collect(probe: &impl Probe, roots: &Roots, sockets: &[PathBuf], now: SystemTime, max_age: u64) -> Report {
    let mut servers = Vec::new();
    for socket in sockets.iter().take(MAX_SERVERS) {
        let observed = tokio::time::timeout(Duration::from_secs(3), inspect(probe, roots, socket, now, max_age)).await;
        servers.push(match observed { Ok(Ok(row)) => row, Ok(Err(e)) => Server::unknown(socket, &e), Err(_) => Server::unknown(socket, "inspection deadline exceeded") });
    }
    // Compare physical directories too: aliases must not bypass server isolation.
    let mut counts = HashMap::new();
    for s in &servers { if let Some(key) = s.save.directory_identity { *counts.entry(key).or_insert(0) += 1; } }
    for s in &mut servers { if s.save.directory_identity.is_some_and(|key| counts.get(&key).is_some_and(|n| *n > 1)) { s.save.state = SaveState::Shared; s.save.reason = "shared snapshot directory: fresh evidence cannot be attributed to this server".into(); } }
    Report { servers }
}
pub(crate) async fn check(guard: &GuardPolicy) -> Report {
    let mut sockets: Vec<PathBuf> = guard.tmux_persistence_sockets.iter().take(MAX_SERVERS).map(|s| crate::policy::expand_tilde(s)).collect();
    if sockets.is_empty() {
        let socket = std::env::var("TMUX").ok().filter(|s| !s.is_empty()).map(|s| PathBuf::from(s.split(',').next().unwrap_or_default())).unwrap_or_else(|| PathBuf::from(std::env::var_os("TMUX_TMPDIR").unwrap_or_else(|| "/tmp".into())).join(format!("tmux-{}/default", unsafe { libc::geteuid() })));
        sockets.push(socket);
    }
    sockets.sort(); sockets.dedup();
    collect(&Tmux, &Roots::host(), &sockets, SystemTime::now(), guard.tmux_persistence_max_save_age_secs.clamp(60, 86400)).await
}
#[cfg(test)]
#[path = "tmux_health_tests.rs"]
mod tests;
