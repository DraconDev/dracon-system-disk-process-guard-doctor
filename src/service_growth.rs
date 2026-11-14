//! Read-only service accounting inside the guard pass. Never invoke admission,
//! recovery, quota, signal or service mutation APIs from this module.
use crate::{policy::GuardPolicy, ProcessIdentity};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    ffi::CString,
    fs::{self, File},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt};

pub(crate) const MAX_TARGETS: usize = 16;
const MAX_HISTORY: usize = 32;
const MAX_OUTPUT: usize = 64 * 1024;
const MIB: u64 = 1024 * 1024;
const PROVENANCE: &str = "cgroup-v2 subtree charged RAM (includes cache/kernel; not RSS/PSS) + swapped bytes; sequential counters";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Manager {
    System,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub(crate) struct Target {
    pub manager: Manager,
    pub unit: String,
}
impl Target {
    fn parse(text: &str) -> Option<Self> {
        let (scope, unit) = text.split_once(':')?;
        let manager = match scope {
            "system" => Manager::System,
            "user" => Manager::User,
            _ => return None,
        };
        if unit.len() > 255
            || unit.starts_with('-')
            || !unit.ends_with(".service")
            || !unit
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-.@:\\".contains(&c))
        {
            return None;
        }
        Some(Self {
            manager,
            unit: unit.into(),
        })
    }
}
pub(crate) fn default_targets() -> Vec<String> {
    [
        "system:dracon-sync-operator.service",
        "user:dracon-system-guard.service",
        "user:xdg-desktop-portal.service",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Unit {
    id: String,
    invocation: String,
    group: String,
    pid: i32,
    load: String,
    active: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Generation {
    boot: String,
    target: Target,
    user_uid: Option<u32>,
    invocation: String,
    group: String,
    device: u64,
    inode: u64,
    pid: i32,
    identity: ProcessIdentity,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Observation {
    pub target: String,
    pub generation: Option<Generation>,
    pub resident_bytes: Option<u64>,
    pub swap_bytes: Option<u64>,
    pub footprint_bytes: Option<u64>,
    pub provenance: &'static str,
    pub status: String,
    pub reason: Option<String>,
    pub elapsed_secs: Option<f64>,
    pub baseline_resident_bytes: Option<u64>,
    pub baseline_swap_bytes: Option<u64>,
    pub growth_bytes: Option<u64>,
    pub growth_mib_per_min: Option<f64>,
}
impl Observation {
    fn unavailable(target: &str, reason: impl Into<String>) -> Self {
        Self {
            target: target.chars().take(280).collect(),
            generation: None,
            resident_bytes: None,
            swap_bytes: None,
            footprint_bytes: None,
            provenance: PROVENANCE,
            status: "unavailable".into(),
            reason: Some(reason.into()),
            elapsed_secs: None,
            baseline_resident_bytes: None,
            baseline_swap_bytes: None,
            growth_bytes: None,
            growth_mib_per_min: None,
        }
    }
}
#[derive(Debug, Serialize)]
pub(crate) struct Report {
    pub enabled: bool,
    pub window_secs: u64,
    pub min_growth_mb: u64,
    pub observations: Vec<Observation>,
}
impl Report {
    pub fn summary(&self) -> String {
        if !self.enabled {
            return "disabled (report only)".into();
        }
        if self.observations.is_empty() {
            return "no targets configured (report only)".into();
        }
        self.observations
            .iter()
            .map(|o| {
                let footprint = o
                    .footprint_bytes
                    .map(|b| format!("{:.1}MiB", b as f64 / MIB as f64))
                    .unwrap_or_else(|| "unknown".into());
                let window = o
                    .elapsed_secs
                    .map(|s| format!(" over {s:.1}s"))
                    .unwrap_or_default();
                let rate = o
                    .growth_mib_per_min
                    .map(|r| format!(", {r:.2}MiB/min"))
                    .unwrap_or_default();
                let reason = o
                    .reason
                    .as_ref()
                    .map(|r| format!(", {r}"))
                    .unwrap_or_default();
                format!(
                    "{}: {} ({footprint}{window}{rate}{reason})",
                    o.target, o.status
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
    pub fn signature(&self) -> String {
        self.observations
            .iter()
            .map(|o| {
                let state = match o.status.as_str() {
                    "sustained_growth" => "sustained_growth",
                    "unavailable" => "unavailable",
                    _ => "observing",
                };
                format!("{}={state}", o.target)
            })
            .collect::<Vec<_>>()
            .join(";")
    }
}
#[derive(Debug, Clone)]
struct Point {
    at: Instant,
    resident: u64,
    swap: u64,
    total: u64,
}
#[derive(Debug)]
struct History {
    generation: Generation,
    last: Point,
    points: VecDeque<Point>,
}
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    histories: HashMap<String, History>,
    settings: Option<(u64, u64)>,
}
impl Tracker {
    pub(crate) fn observe(
        &mut self,
        guard: &GuardPolicy,
        mut rows: Vec<Observation>,
        now: Instant,
    ) -> Report {
        let settings = (
            guard.service_memory_growth_window_secs.clamp(60, 86_400),
            guard.service_memory_growth_min_mb.max(1),
        );
        if self.settings != Some(settings) || !guard.monitor_service_memory_growth {
            self.histories.clear();
        }
        self.settings = Some(settings);
        if !guard.monitor_service_memory_growth {
            rows.clear();
        }
        rows.truncate(MAX_TARGETS);
        self.histories
            .retain(|key, _| rows.iter().any(|r| &r.target == key));
        let window = Duration::from_secs(settings.0);
        let spacing = Duration::from_secs(settings.0.div_ceil((MAX_HISTORY - 2) as u64));
        for row in &mut rows {
            let (Some(generation), Some(resident), Some(swap), Some(total)) = (
                row.generation.clone(),
                row.resident_bytes,
                row.swap_bytes,
                row.footprint_bytes,
            ) else {
                self.histories.remove(&row.target);
                continue;
            };
            let point = Point {
                at: now,
                resident,
                swap,
                total,
            };
            let mut reset = None;
            if let Some(h) = self.histories.get(&row.target) {
                reset = if h.generation != generation {
                    Some("service/process incarnation changed")
                } else if now <= h.last.at {
                    Some("non-increasing observation clock")
                } else if now.duration_since(h.last.at) > window / 2 {
                    Some("sampling gap")
                } else if total < h.last.total {
                    Some("footprint decreased (release/reset)")
                } else {
                    None
                };
            }
            if reset.is_some() {
                self.histories.remove(&row.target);
            }
            let h = self
                .histories
                .entry(row.target.clone())
                .or_insert_with(|| History {
                    generation,
                    last: point.clone(),
                    points: VecDeque::from([point.clone()]),
                });
            h.last = point.clone();
            if now.duration_since(h.points.back().unwrap().at) >= spacing {
                h.points.push_back(point);
            }
            while h.points.len() > 1 && now.duration_since(h.points[1].at) >= window {
                h.points.pop_front();
            }
            while h.points.len() > MAX_HISTORY {
                h.points.pop_front();
            }
            let baseline = h.points.front().unwrap();
            let elapsed = now.duration_since(baseline.at).as_secs_f64();
            let delta = total.saturating_sub(baseline.total);
            row.elapsed_secs = Some(elapsed);
            row.baseline_resident_bytes = Some(baseline.resident);
            row.baseline_swap_bytes = Some(baseline.swap);
            row.growth_bytes = Some(delta);
            row.growth_mib_per_min =
                (elapsed > 0.0).then(|| delta as f64 / MIB as f64 * 60.0 / elapsed);
            let midpoint = baseline.at + Duration::from_secs_f64(elapsed / 2.0);
            // Balanced growth, not a dominant burst plus negligible drift:
            // each half contributes >= one quarter of BOTH the observed delta
            // and the configured minimum. Conservative, never a leak claim.
            let meaningful_half = delta
                .div_ceil(4)
                .max(settings.1.saturating_mul(MIB).div_ceil(4));
            let both_halves = h
                .points
                .iter()
                .rev()
                .find(|p| p.at <= midpoint)
                .is_some_and(|middle| {
                    middle.total.saturating_sub(baseline.total) >= meaningful_half
                        && total.saturating_sub(middle.total) >= meaningful_half
                });
            row.status = if reset.is_some() {
                "reset"
            } else if elapsed < window.as_secs_f64() {
                "warming_up"
            } else if delta >= settings.1.saturating_mul(MIB) && both_halves {
                "sustained_growth"
            } else {
                "stable"
            }
            .into();
            row.reason = reset.map(str::to_string);
        }
        Report {
            enabled: guard.monitor_service_memory_growth,
            window_secs: settings.0,
            min_growth_mb: settings.1,
            observations: rows,
        }
    }
}

trait Probe {
    async fn properties(&self, manager: Manager, units: &[String]) -> Result<String, String>;
}
struct Systemd;
pub(crate) async fn capped_output(
    reader: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(std::io::Error::other("manager output limit exceeded"));
    }
    Ok(bytes)
}
fn manager_command(bin: &str, manager: Manager, units: &[String]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(bin);
    command
        .args([
            if manager == Manager::User {
                "--user"
            } else {
                "--system"
            },
            "show",
            "--no-pager",
            "--property=Id,LoadState,ActiveState,InvocationID,ControlGroup,MainPID",
            "--",
        ])
        .args(units)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}
impl Probe for Systemd {
    async fn properties(&self, manager: Manager, units: &[String]) -> Result<String, String> {
        let bin = crate::resolve_bin_strict("systemctl").map_err(|e| e.to_string())?;
        let mut child = manager_command(&bin, manager, units)
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdout = child.stdout.take().ok_or("manager stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("manager stderr unavailable")?;
        let result = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::try_join!(
                capped_output(stdout, MAX_OUTPUT),
                capped_output(stderr, 4096),
                child.wait()
            )
        })
        .await
        .map_err(|_| "manager read timed out")?
        .map_err(|e| e.to_string())?;
        if !result.2.success() {
            return Err(format!("manager show failed: {}", result.2));
        }
        String::from_utf8(result.0).map_err(|_| "manager output is not UTF-8".into())
    }
}
fn parse_units(text: &str) -> Result<BTreeMap<String, Unit>, String> {
    if text.len() > MAX_OUTPUT {
        return Err("manager output limit exceeded".into());
    }
    let mut result = BTreeMap::new();
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if fields.is_empty() {
                continue;
            }
            let get = |key: &str| {
                fields
                    .get(key)
                    .cloned()
                    .ok_or_else(|| format!("missing {key}"))
            };
            let unit = Unit {
                id: get("Id")?,
                invocation: get("InvocationID")?,
                group: get("ControlGroup")?,
                pid: get("MainPID")?
                    .parse::<i32>()
                    .map_err(|_| "invalid MainPID")?,
                load: get("LoadState")?,
                active: get("ActiveState")?,
            };
            if result.len() >= MAX_TARGETS || result.insert(unit.id.clone(), unit).is_some() {
                return Err("duplicate/too many unit identities".into());
            }
            fields.clear();
        } else {
            let (key, value) = line.split_once('=').ok_or("malformed manager property")?;
            if fields.insert(key.to_string(), value.to_string()).is_some() {
                return Err("duplicate manager property".into());
            }
        }
    }
    Ok(result)
}

struct Roots {
    proc: PathBuf,
    cgroup: PathBuf,
    verify_host: bool,
}
impl Roots {
    fn host() -> Self {
        Self {
            proc: "/proc".into(),
            cgroup: "/sys/fs/cgroup".into(),
            verify_host: true,
        }
    }
}
fn bounded_text(path: &Path, limit: usize) -> Result<String, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    text_from(file, limit)
}
fn text_from(file: File, limit: usize) -> Result<String, String> {
    let mut text = String::new();
    file.take((limit + 1) as u64)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() > limit {
        return Err("file byte limit exceeded".into());
    }
    Ok(text)
}
fn components(group: &str) -> Result<Vec<&str>, String> {
    if !group.starts_with('/') || group == "/" || group.len() > 4096 {
        return Err("unsafe/root ControlGroup".into());
    }
    let parts: Vec<_> = group[1..].split('/').collect();
    if parts.len() > 32
        || parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == ".." || p.len() > 255)
    {
        return Err("unsafe ControlGroup components".into());
    }
    Ok(parts)
}
fn open_beneath(roots: &Roots, group: &str) -> Result<File, String> {
    let path =
        CString::new(roots.cgroup.as_os_str().as_bytes()).map_err(|_| "invalid cgroup root")?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // Owned descriptors are retained through the measurement; no symlink walk.
    let fd = unsafe { libc::open(path.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut dir = unsafe { File::from_raw_fd(fd) };
    if roots.verify_host {
        let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::fstatfs(dir.as_raw_fd(), info.as_mut_ptr()) } != 0
            || unsafe { info.assume_init() }.f_type != 0x63677270
        {
            return Err("cgroup-v2 filesystem unavailable".into());
        }
    }
    for part in components(group)? {
        let name = CString::new(part).map_err(|_| "invalid cgroup component")?;
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        dir = unsafe { File::from_raw_fd(fd) };
    }
    Ok(dir)
}
fn counter(dir: &File, name: &str) -> Result<u64, String> {
    let name = CString::new(name).map_err(|_| "invalid counter name")?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("counter is not a regular file".into());
    }
    text_from(file, 64)?
        .trim()
        .parse()
        .map_err(|_| "invalid memory counter".into())
}
fn main_group(roots: &Roots, pid: i32) -> Result<String, String> {
    let text = bounded_text(&roots.proc.join(pid.to_string()).join("cgroup"), 4096)?;
    let mut groups = text.lines().filter_map(|l| l.strip_prefix("0::"));
    let group = groups
        .next()
        .ok_or("unified process cgroup unavailable")?
        .to_string();
    if groups.next().is_some() {
        return Err("ambiguous process cgroup".into());
    }
    components(&group)?;
    Ok(group)
}
fn sample_unit(
    roots: &Roots,
    target: &Target,
    unit: &Unit,
    boot: &str,
) -> Result<Observation, String> {
    if unit.id != target.unit
        || unit.load != "loaded"
        || unit.active != "active"
        || unit.pid <= 0
        || unit.invocation.len() != 32
        || !unit.invocation.bytes().all(|b| b.is_ascii_hexdigit())
        || unit.invocation.bytes().all(|b| b == b'0')
    {
        return Err("active service incarnation unavailable".into());
    }
    if target.manager == Manager::User {
        let prefix = format!(
            "/user.slice/user-{}.slice/user@{}.service/",
            unsafe { libc::geteuid() },
            unsafe { libc::geteuid() }
        );
        if !unit.group.starts_with(&prefix) {
            return Err("user-manager cgroup/UID mismatch".into());
        }
    }
    let identity = crate::read_proc_identity(&roots.proc, unit.pid)
        .map_err(|_| "main-process identity unavailable")?;
    let membership = main_group(roots, unit.pid)?;
    if membership != unit.group && !membership.starts_with(&(unit.group.clone() + "/")) {
        return Err("main-process ControlGroup mismatch".into());
    }
    let dir = open_beneath(roots, &unit.group)?;
    let meta = dir.metadata().map_err(|e| e.to_string())?;
    let resident = counter(&dir, "memory.current");
    let swap = counter(&dir, "memory.swap.current");
    let after = open_beneath(roots, &unit.group)?
        .metadata()
        .map_err(|e| e.to_string())?;
    if (meta.dev(), meta.ino()) != (after.dev(), after.ino())
        || crate::read_proc_identity(&roots.proc, unit.pid)
            .ok()
            .as_ref()
            != Some(&identity)
        || main_group(roots, unit.pid)? != membership
    {
        return Err("process/cgroup changed during measurement".into());
    }
    let reason = match (&resident, &swap) {
        (Err(_), _) => Some("resident accounting unavailable".into()),
        (_, Err(_)) => Some("swap accounting unavailable".into()),
        _ => None,
    };
    let resident_bytes = resident.ok();
    let swap_bytes = swap.ok();
    let footprint_bytes = resident_bytes
        .zip(swap_bytes)
        .and_then(|(r, s)| r.checked_add(s));
    let mut row = Observation::unavailable(
        &format!(
            "{}:{}",
            if target.manager == Manager::System {
                "system"
            } else {
                "user"
            },
            target.unit
        ),
        "warming up",
    );
    row.generation = Some(Generation {
        boot: boot.into(),
        target: target.clone(),
        user_uid: (target.manager == Manager::User).then(|| unsafe { libc::geteuid() }),
        invocation: unit.invocation.clone(),
        group: unit.group.clone(),
        device: meta.dev(),
        inode: meta.ino(),
        pid: unit.pid,
        identity,
    });
    row.resident_bytes = resident_bytes;
    row.swap_bytes = swap_bytes;
    row.footprint_bytes = footprint_bytes;
    row.reason = reason.or_else(|| {
        footprint_bytes
            .is_none()
            .then(|| "footprint unavailable/overflow".into())
    });
    Ok(row)
}
async fn properties(
    probe: &impl Probe,
    targets: &[Target],
) -> BTreeMap<Manager, Result<BTreeMap<String, Unit>, String>> {
    let mut result = BTreeMap::new();
    for manager in [Manager::System, Manager::User] {
        let units: Vec<_> = targets
            .iter()
            .filter(|t| t.manager == manager)
            .map(|t| t.unit.clone())
            .collect();
        if !units.is_empty() {
            result.insert(
                manager,
                probe
                    .properties(manager, &units)
                    .await
                    .and_then(|s| parse_units(&s)),
            );
        }
    }
    result
}
/// Whether this process sees the host's PID and cgroup namespaces.
///
/// FIXED 2026-11-14: the check compared `self/ns/*` with `/proc/1/ns/*`
/// only. Reading another process's namespace links needs ptrace access,
/// which the guard (an unprivileged user service whose bounding set is only
/// CAP_SYS_NICE) has for neither pid 1 nor its own systemd manager (which
/// holds CAP_WAKE_ALARM) — so every measurement was "unavailable", forever.
///
/// When pid 1 cannot be read, the proof comes from facts about OURSELVES,
/// which are always readable:
/// - PID: `NSpid:` in `self/status` has exactly one entry only in the
///   initial PID namespace (one pid per nesting level otherwise);
/// - cgroup: in the host cgroup namespace `self/cgroup` shows the full
///   path from the root (`/user.slice/…`, `/system.slice/…`,
///   `/init.scope`); inside a nested one it is relative (`/`, `/sub`).
///
/// Anything unreadable or unrecognised still fails closed.
pub(crate) fn shares_host_namespaces(proc_root: &Path) -> bool {
    let init_readable = ["pid", "cgroup"]
        .iter()
        .all(|ns| fs::read_link(proc_root.join(format!("1/ns/{ns}"))).is_ok());
    if init_readable {
        return ["pid", "cgroup"].iter().all(|ns| {
            let ours = fs::read_link(proc_root.join(format!("self/ns/{ns}")));
            let init = fs::read_link(proc_root.join(format!("1/ns/{ns}")));
            matches!((ours, init), (Ok(a), Ok(b)) if a == b)
        });
    }
    let initial_pid_ns = fs::read_to_string(proc_root.join("self/status"))
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|l| l.strip_prefix("NSpid:"))
                .map(|v| v.split_whitespace().count() == 1)
        })
        .unwrap_or(false);
    let host_cgroup_ns = fs::read_to_string(proc_root.join("self/cgroup"))
        .ok()
        .and_then(|raw| {
            raw.lines().find_map(|l| l.strip_prefix("0::")).map(|path| {
                let path = path.trim();
                path.starts_with("/user.slice/")
                    || path.starts_with("/system.slice/")
                    || path == "/init.scope"
            })
        })
        .unwrap_or(false);
    initial_pid_ns && host_cgroup_ns
}

async fn collect(probe: &impl Probe, roots: &Roots, names: &[String]) -> Vec<Observation> {
    if names.is_empty() {
        return Vec::new();
    }
    let unavailable = |reason: &str| {
        names
            .iter()
            .take(MAX_TARGETS)
            .map(|n| Observation::unavailable(n, reason))
            .collect::<Vec<_>>()
    };
    let boot = match bounded_text(&roots.proc.join("sys/kernel/random/boot_id"), 64) {
        Ok(b) if crate::oom_job::valid_id(b.trim()) => b.trim().to_string(),
        _ => return unavailable("boot identity unavailable"),
    };
    if roots.verify_host && !shares_host_namespaces(&roots.proc) {
        return unavailable("host PID/cgroup namespace visibility unavailable");
    }
    let targets: Vec<_> = names
        .iter()
        .take(MAX_TARGETS)
        .filter_map(|s| Target::parse(s))
        .collect();
    let before = properties(probe, &targets).await;
    let mut rows = Vec::new();
    for name in names.iter().take(MAX_TARGETS) {
        let measured = Target::parse(name)
            .ok_or_else(|| "invalid canonical service target".to_string())
            .and_then(|target| {
                let units = before
                    .get(&target.manager)
                    .ok_or("manager unavailable")?
                    .as_ref()
                    .map_err(Clone::clone)?;
                let unit = units
                    .get(&target.unit)
                    .ok_or("service identity unavailable")?;
                sample_unit(roots, &target, unit, &boot)
            });
        rows.push(measured.unwrap_or_else(|e| Observation::unavailable(name, e)));
    }
    let after = properties(probe, &targets).await;
    for row in &mut rows {
        if let Some(generation) = &row.generation {
            let target = &generation.target;
            let a = before
                .get(&target.manager)
                .and_then(|r| r.as_ref().ok())
                .and_then(|u| u.get(&target.unit));
            let b = after
                .get(&target.manager)
                .and_then(|r| r.as_ref().ok())
                .and_then(|u| u.get(&target.unit));
            let membership = main_group(roots, generation.pid).is_ok_and(|group| {
                group == generation.group || group.starts_with(&(generation.group.clone() + "/"))
            });
            let directory = open_beneath(roots, &generation.group)
                .ok()
                .and_then(|d| d.metadata().ok())
                .is_some_and(|m| m.dev() == generation.device && m.ino() == generation.inode);
            let current = crate::read_proc_identity(&roots.proc, generation.pid)
                .ok()
                .as_ref()
                == Some(&generation.identity);
            if a != b || b.is_none() || !current || !directory || !membership {
                *row = Observation::unavailable(
                    &row.target,
                    "manager/process/cgroup incarnation changed/unavailable during measurement",
                );
            }
        }
    }
    rows
}
pub(crate) async fn check(guard: &GuardPolicy, tracker: &mut Tracker) -> Report {
    if !guard.monitor_service_memory_growth {
        return tracker.observe(guard, Vec::new(), Instant::now());
    }
    let rows = tokio::time::timeout(
        Duration::from_secs(5),
        collect(
            &Systemd,
            &Roots::host(),
            &guard.service_memory_growth_targets,
        ),
    )
    .await
    .unwrap_or_else(|_| {
        guard
            .service_memory_growth_targets
            .iter()
            .take(MAX_TARGETS)
            .map(|n| Observation::unavailable(n, "service sampling deadline exceeded"))
            .collect()
    });
    tracker.observe(guard, rows, Instant::now())
}

#[cfg(test)]
#[path = "service_growth_tests.rs"]
mod tests;
