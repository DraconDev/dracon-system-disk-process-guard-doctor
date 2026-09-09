//! Event system for dracon-system — structured event logging and persistence.

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::Serialize;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// Keep one active segment and one rotated segment. The record cap also keeps a
// single unusually large error message from defeating the storage bound.
const MAX_EVENT_LOG_BYTES: u64 = 10 * 1024 * 1024;
const MAX_EVENT_RECORD_BYTES: usize = 64 * 1024;
const MAX_EVENT_TAIL_LINES: usize = 1_000;
const MAX_EVENT_TAIL_BYTES: usize = MAX_EVENT_LOG_BYTES as usize;

static ROLLING_LOG: std::sync::OnceLock<Mutex<Vec<String>>> = std::sync::OnceLock::new();

fn get_log() -> &'static Mutex<Vec<String>> {
    ROLLING_LOG.get_or_init(|| Mutex::new(Vec::new()))
}

/// Severity levels for dracon events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EventSeverity {
    /// Normal operational information.
    Info,
    /// Warning about a potential issue.
    Warn,
    /// Error that affected an operation.
    Error,
}

/// A structured event emitted by dracon services.
#[derive(Debug, Clone, Serialize)]
pub struct DraconEvent {
    /// Source domain (e.g., "system", "sync", "warden").
    pub domain: String,
    /// Event severity level.
    pub severity: EventSeverity,
    /// Filesystem path related to the event.
    pub path: String,
    /// Human-readable event message.
    pub message: String,
    /// RFC 3339 timestamp.
    pub timestamp: String,
}

impl DraconEvent {
    /// Create a new event with the given domain, severity, path, and message.
    pub fn new<T1: ToString, T2: ToString, T3: ToString>(
        domain: T1,
        severity: EventSeverity,
        path: T2,
        message: T3,
    ) -> Self {
        Self {
            domain: domain.to_string(),
            severity,
            path: path.to_string(),
            message: message.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let suffix = "…";
    if max_bytes < suffix.len() {
        let mut end = max_bytes;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        return value[..end].to_string();
    }

    let mut end = max_bytes - suffix.len();
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &value[..end], suffix)
}

fn serialize_event_for_storage(event: &DraconEvent) -> Option<String> {
    let json = serde_json::to_string(event).ok()?;
    if json.len() <= MAX_EVENT_RECORD_BYTES {
        return Some(json);
    }

    // Error messages and paths are normally short, but they can contain
    // command output supplied by another process. Preserve a valid, useful
    // record without allowing one event to defeat rotation.
    let bounded = DraconEvent {
        domain: truncate_utf8(&event.domain, 512),
        severity: event.severity,
        path: truncate_utf8(&event.path, 2_048),
        message: truncate_utf8(&event.message, 4_096),
        timestamp: truncate_utf8(&event.timestamp, 128),
    };
    let json = serde_json::to_string(&bounded).ok()?;
    (json.len() <= MAX_EVENT_RECORD_BYTES).then_some(json)
}

fn event_lock_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("events.jsonl");
    path.with_file_name(format!("{name}.lock"))
}

fn rotated_event_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("events.jsonl");
    path.with_file_name(format!("{name}.1"))
}

fn rotate_event_log(path: &Path) -> std::io::Result<()> {
    let rotated = rotated_event_path(path);
    match fs::rename(path, &rotated) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // `rename` replaces an existing file on Unix, while Windows needs
            // the destination removed first. The caller holds the lock file.
            fs::remove_file(&rotated)?;
            fs::rename(path, rotated)
        }
        Err(error) => Err(error),
    }
}

fn discard_oversized_segment(path: &Path) -> std::io::Result<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.len() > MAX_EVENT_LOG_BYTES => fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn persist_event(path: &Path, json: &str) -> std::io::Result<()> {
    if json.len() > MAX_EVENT_RECORD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "serialized event exceeds the record limit",
        ));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(event_lock_path(path))?;
    lock_file.lock_exclusive()?;

    let result = (|| {
        // Clean up files created by the pre-rotation implementation before
        // deciding whether the current segment can be retained.
        discard_oversized_segment(&rotated_event_path(path))?;
        let current_size = match fs::metadata(path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        let append_size = u64::try_from(json.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        if current_size > MAX_EVENT_LOG_BYTES {
            // Do not move an unbounded legacy log into the retained segment.
            fs::remove_file(path)?;
        } else if current_size > 0 && current_size.saturating_add(append_size) > MAX_EVENT_LOG_BYTES
        {
            rotate_event_log(path)?;
        }

        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(json.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()
    })();
    let unlock_result = lock_file.unlock();

    match (result, unlock_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// Emit an event: log to the rolling buffer, print to stderr, and persist to JSONL.
pub fn emit_event(event: &DraconEvent) {
    if let Ok(mut log) = get_log().lock() {
        if log.len() >= 1000 {
            log.remove(0);
        }
        log.push(format!(
            "[{}] {:?}: {} - {}",
            event.timestamp, event.severity, event.path, event.message
        ));
    }
    eprintln!(
        "[{}] {:?}: {} - {}",
        event.timestamp, event.severity, event.path, event.message
    );
    let events_path = events_path();
    if let Some(json) = serialize_event_for_storage(event) {
        if let Err(error) = persist_event(&events_path, &json) {
            eprintln!(
                "⚠️ failed to persist event to {}: {}",
                events_path.display(),
                error
            );
        }
    }
}

fn append_bounded_line_bytes(line: &mut Vec<u8>, too_long: &mut bool, bytes: &[u8]) {
    if *too_long {
        return;
    }
    let remaining = MAX_EVENT_RECORD_BYTES.saturating_sub(line.len());
    if bytes.len() > remaining {
        line.extend_from_slice(&bytes[..remaining]);
        *too_long = true;
    } else {
        line.extend_from_slice(bytes);
    }
}

fn retain_event_line(
    retained: &mut VecDeque<String>,
    retained_bytes: &mut usize,
    line: String,
    tail: usize,
) {
    if tail == 0 {
        return;
    }
    *retained_bytes = retained_bytes.saturating_add(line.len());
    retained.push_back(line);
    while retained.len() > tail || *retained_bytes > MAX_EVENT_TAIL_BYTES {
        if let Some(old) = retained.pop_front() {
            *retained_bytes = retained_bytes.saturating_sub(old.len());
        }
    }
}

fn discard_partial_line<R: BufRead>(reader: &mut R) -> std::io::Result<()> {
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(());
        }
        if let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
            reader.consume(newline + 1);
            return Ok(());
        }
        let length = buffer.len();
        reader.consume(length);
    }
}

fn read_tail_from_reader<R: BufRead>(
    reader: &mut R,
    tail: usize,
    retained: &mut VecDeque<String>,
    retained_bytes: &mut usize,
) -> std::io::Result<usize> {
    let mut current = Vec::new();
    let mut current_too_long = false;
    let mut total_lines = 0usize;

    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        if let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
            append_bounded_line_bytes(&mut current, &mut current_too_long, &buffer[..newline]);
            reader.consume(newline + 1);
            total_lines = total_lines.saturating_add(1);

            if !current_too_long {
                let mut line = std::mem::take(&mut current);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Ok(line) = String::from_utf8(line) {
                    retain_event_line(retained, retained_bytes, line, tail);
                }
            } else {
                current.clear();
            }
            current_too_long = false;
        } else {
            let length = buffer.len();
            append_bounded_line_bytes(&mut current, &mut current_too_long, buffer);
            reader.consume(length);
        }
    }

    if !current.is_empty() || current_too_long {
        total_lines = total_lines.saturating_add(1);
        if !current_too_long {
            if let Some(&b'\r') = current.last() {
                current.pop();
            }
            if let Ok(line) = String::from_utf8(current) {
                retain_event_line(retained, retained_bytes, line, tail);
            }
        }
    }

    Ok(total_lines)
}

/// Stream the event segments while retaining only a bounded recent tail.
fn read_tail_segments(paths: &[&Path], tail: usize) -> Result<(Vec<String>, usize)> {
    let Some(lock_target) = paths.last() else {
        return Ok((Vec::new(), 0));
    };
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(event_lock_path(lock_target))?;
    lock_file.lock_shared()?;

    let result = (|| {
        let tail = tail.min(MAX_EVENT_TAIL_LINES);
        let mut retained = VecDeque::new();
        let mut retained_bytes = 0usize;
        let mut total_lines = 0usize;

        for path in paths {
            let file_len = match fs::metadata(path) {
                Ok(metadata) => metadata.len(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let mut file = match File::open(path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let read_bytes = file_len.min(MAX_EVENT_TAIL_BYTES as u64);
            let start = file_len.saturating_sub(read_bytes);
            file.seek(SeekFrom::Start(start))?;
            let mut reader = BufReader::new(file.take(read_bytes));
            if start > 0 {
                // A bounded window may begin in the middle of a JSONL record;
                // discard that partial record rather than parsing corrupt JSON.
                discard_partial_line(&mut reader)?;
            }
            total_lines = total_lines.saturating_add(read_tail_from_reader(
                &mut reader,
                tail,
                &mut retained,
                &mut retained_bytes,
            )?);
        }

        Ok((retained.into_iter().collect(), total_lines))
    })();
    let unlock_result = lock_file.unlock();

    match (result, unlock_result) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn read_tail_lines(path: &Path, tail: usize) -> Result<(Vec<String>, usize)> {
    read_tail_segments(&[path], tail)
}

/// Returns the path to the shared events JSONL file.
pub(crate) fn events_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".dracon/events.jsonl"))
        .unwrap_or_else(|| PathBuf::from("/tmp/dracon-events.jsonl"))
}

/// Display recent events with filtering and deduplication.
pub(crate) fn cmd_events(
    tail: usize,
    source: Option<String>,
    severity: Option<String>,
    dedup: bool,
    json_output: bool,
) -> Result<()> {
    use comfy_table::{
        presets::UTF8_FULL_CONDENSED, Attribute, Cell, Color, ContentArrangement, Table,
    };

    let path = events_path();
    let rotated_path = rotated_event_path(&path);
    if !path.exists() && !rotated_path.exists() {
        if !json_output {
            println!("No events found ({} does not exist)", path.display());
        }
        return Ok(());
    }
    // Rotation moves the older active segment to `.1`; read it first so the
    // combined tail remains chronological, then read the current segment.
    let segment_paths = [rotated_path.as_path(), path.as_path()];
    let (lines, total_lines) = read_tail_segments(&segment_paths, tail)?;

    let mut parsed: Vec<serde_json::Value> = Vec::new();
    for line in &lines {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(ref s) = source {
                if val.get("domain").and_then(|v| v.as_str()) != Some(s.as_str()) {
                    continue;
                }
            }
            let sev_lower = severity.as_deref().map(|s| s.to_lowercase());
            if let Some(ref sl) = sev_lower {
                let val_sev = val
                    .get("severity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                if val_sev != *sl {
                    continue;
                }
            }
            parsed.push(val);
        }
    }

    if dedup {
        parsed.dedup_by(|a, b| {
            a.get("domain") == b.get("domain")
                && a.get("severity") == b.get("severity")
                && a.get("path") == b.get("path")
                && a.get("message") == b.get("message")
        });
    }

    if json_output {
        // JSONL represents an empty result as an empty stream. Keep the
        // human-only no-match sentinel out of machine-readable output.
        for ev in &parsed {
            println!("{}", serde_json::to_string(ev).unwrap_or_default());
        }
        return Ok(());
    }

    if parsed.is_empty() {
        println!("(no matching events)");
        return Ok(());
    }

    // ---- Severity count buckets (for the summary line) ----
    let mut sev_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for ev in &parsed {
        let sev = ev
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("info");
        *sev_counts.entry(sev.to_lowercase()).or_insert(0) += 1;
    }
    let total = parsed.len();
    let mut sev_parts: Vec<String> = Vec::new();
    for (name, n) in &sev_counts {
        if *n > 0 {
            sev_parts.push(format!("{} {}", n, name));
        }
    }
    let sev_summary = if sev_parts.is_empty() {
        String::new()
    } else {
        format!(" · {}", sev_parts.join(" · "))
    };

    // ---- Summary line (one-liner with count + severity mix) ----
    let filter_note = if source.is_some() || severity.is_some() {
        format!(" (showing {} of {} events)", total, total_lines)
    } else {
        String::new()
    };
    println!("📒 Events{filter_note}: {total} total{sev_summary}",);

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("SEV"),
            Cell::new("DOMAIN"),
            Cell::new("PATH"),
            Cell::new("MESSAGE"),
            Cell::new("TIME"),
        ]);

    for ev in &parsed {
        let sev = ev.get("severity").and_then(|v| v.as_str()).unwrap_or("-");
        let domain = ev.get("domain").and_then(|v| v.as_str()).unwrap_or("-");
        let evpath = ev.get("path").and_then(|v| v.as_str()).unwrap_or("-");
        let message = ev.get("message").and_then(|v| v.as_str()).unwrap_or("-");
        let ts = ev.get("timestamp").and_then(|v| v.as_str()).unwrap_or("-");

        let ts_short = shorten_event_time(ts);

        let (sev_str, sev_color) = match sev.to_lowercase().as_str() {
            "error" | "critical" => (sev, Color::Red),
            "warn" | "warning" => (sev, Color::Yellow),
            _ => (sev, Color::Green),
        };

        table.add_row(vec![
            Cell::new(sev_str)
                .fg(sev_color)
                .add_attribute(Attribute::Bold),
            Cell::new(domain),
            Cell::new(evpath),
            Cell::new(message),
            Cell::new(ts_short),
        ]);
    }

    println!("{table}");
    // Footer with severity mix (only non-zero buckets)
    if !sev_parts.is_empty() {
        println!("Total: {} event(s){}", total, sev_summary);
    } else {
        println!("Total: {} event(s)", total);
    }
    Ok(())
}

/// Format an RFC 3339 timestamp as a human-readable relative time.
pub(crate) fn shorten_event_time(ts: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
        let now = chrono::Utc::now();
        let diff = now.signed_duration_since(dt);
        if diff.num_seconds() < 0 {
            return "just now".to_string();
        }
        let mins = diff.num_minutes();
        if mins < 1 {
            return format!("{}s", diff.num_seconds());
        }
        if mins < 60 {
            return format!("{mins}m");
        }
        let hours = diff.num_hours();
        if hours < 24 {
            return format!("{hours}h");
        }
        let days = diff.num_days();
        if days < 30 {
            return format!("{days}d");
        }
        return format!("{}mo", days / 30);
    }
    if ts.len() > 19 {
        // Keep the display fallback Unicode-safe. Byte slicing at an
        // arbitrary offset can panic when malformed/non-RFC input contains
        // a multibyte character near the cutoff.
        ts.chars().take(19).collect()
    } else {
        ts.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_event_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "dracon-system-events-{label}-{}-{nanos}.jsonl",
            std::process::id()
        ))
    }

    #[test]
    fn persistent_event_log_rotates_before_append() {
        let path = temp_event_path("rotation");
        let rotated = rotated_event_path(&path);
        fs::write(&path, vec![b'x'; MAX_EVENT_LOG_BYTES as usize]).expect("write full log");

        persist_event(&path, r#"{"message":"new"}"#).expect("rotate and append");

        assert_eq!(
            fs::read_to_string(&path).expect("read active log"),
            "{\"message\":\"new\"}\n"
        );
        assert_eq!(
            fs::metadata(&rotated).expect("rotated log").len(),
            MAX_EVENT_LOG_BYTES
        );
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&rotated);
        let _ = fs::remove_file(event_lock_path(&path));
    }

    #[test]
    fn oversized_legacy_segments_are_discarded_before_new_events() {
        let path = temp_event_path("legacy");
        let rotated = rotated_event_path(&path);
        fs::write(&path, vec![b'x'; MAX_EVENT_LOG_BYTES as usize + 1])
            .expect("write oversized active log");
        fs::write(&rotated, vec![b'y'; MAX_EVENT_LOG_BYTES as usize + 1])
            .expect("write oversized rotated log");

        persist_event(&path, r#"{"message":"new"}"#).expect("replace legacy logs");

        assert_eq!(
            fs::read_to_string(&path).expect("read active log"),
            "{\"message\":\"new\"}\n"
        );
        assert!(
            !rotated.exists(),
            "oversized legacy backup must not survive"
        );
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(event_lock_path(&path));
    }

    #[test]
    fn rotated_segment_is_included_in_chronological_tail() {
        let path = temp_event_path("rotated-tail");
        let rotated = rotated_event_path(&path);
        fs::write(&rotated, "{\"index\":1}\n").expect("write rotated log");
        fs::write(&path, "{\"index\":2}\n").expect("write active log");

        let (lines, total) =
            read_tail_segments(&[rotated.as_path(), path.as_path()], 2).expect("read event tail");

        assert_eq!(total, 2);
        assert_eq!(lines, vec![r#"{"index":1}"#, r#"{"index":2}"#]);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&rotated);
        let _ = fs::remove_file(event_lock_path(&path));
    }

    #[test]
    fn concurrent_persistence_keeps_each_record_as_a_json_line() {
        let path = temp_event_path("concurrent");
        std::thread::scope(|scope| {
            let path = path.as_path();
            for worker in 0..4 {
                scope.spawn(move || {
                    for sequence in 0..25 {
                        let json = format!(r#"{{"worker":{worker},"sequence":{sequence}}}"#);
                        persist_event(path, &json).expect("persist concurrent event");
                    }
                });
            }
        });

        let (lines, total) = read_tail_lines(&path, 100).expect("read concurrent event log");
        assert_eq!(total, 100);
        assert_eq!(lines.len(), 100);
        for line in lines {
            let _: serde_json::Value = serde_json::from_str(&line).expect("valid JSON line");
        }
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(event_lock_path(&path));
    }

    #[test]
    fn persistence_failure_is_reported_without_writing_outside_the_log() {
        let blocker = temp_event_path("failure");
        fs::write(&blocker, b"not a directory").expect("write blocker");
        let path = blocker.join("events.jsonl");

        assert!(persist_event(&path, r#"{"message":"event"}"#).is_err());

        let _ = fs::remove_file(&blocker);
    }

    #[test]
    fn oversized_event_is_truncated_to_a_valid_bounded_record() {
        let event = DraconEvent::new(
            "domain".repeat(10_000),
            EventSeverity::Error,
            "/tmp/".to_string() + &"path".repeat(10_000),
            "failure ".repeat(50_000),
        );

        let json = serialize_event_for_storage(&event).expect("serialize bounded event");
        assert!(json.len() <= MAX_EVENT_RECORD_BYTES);
        let _: serde_json::Value = serde_json::from_str(&json).expect("valid JSON record");
    }

    #[test]
    fn tail_reader_retains_requested_lines_without_loading_the_file() {
        let path = temp_event_path("tail");
        let content = (0..5)
            .map(|index| format!("{{\"index\":{index}}}\n"))
            .collect::<String>();
        fs::write(&path, content).expect("write event log");

        let (lines, total) = read_tail_lines(&path, 2).expect("read event tail");

        assert_eq!(total, 5);
        assert_eq!(lines, vec![r#"{"index":3}"#, r#"{"index":4}"#]);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(event_lock_path(&path));
    }

    #[test]
    fn tail_reader_skips_oversized_lines_and_keeps_memory_bounded() {
        let path = temp_event_path("oversized-line");
        let mut content = vec![b'x'; MAX_EVENT_RECORD_BYTES + 1];
        content.extend_from_slice(b"\n{\"kept\":true}\n");
        fs::write(&path, content).expect("write oversized event log");

        let (lines, total) = read_tail_lines(&path, 2).expect("read event tail");

        assert_eq!(total, 2);
        assert_eq!(lines, vec![r#"{"kept":true}"#]);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(event_lock_path(&path));
    }
}
