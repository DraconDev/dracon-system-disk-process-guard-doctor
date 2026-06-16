//! Event system for dracon-system — structured event logging and persistence.

use anyhow::{Context, Result};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

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
    if let Some(events_path) = dirs::home_dir().map(|h| h.join(".dracon/events.jsonl")) {
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&events_path)
        {
            if let Ok(json) = serde_json::to_string(event) {
                let _ = writeln!(file, "{}", json);
            }
        }
    }
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
    if !path.exists() {
        println!("No events found ({} does not exist)", path.display());
        return Ok(());
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let lines: Vec<&str> = contents.lines().collect();
    let start = if lines.len() > tail {
        lines.len() - tail
    } else {
        0
    };

    let mut parsed: Vec<serde_json::Value> = Vec::new();
    for line in &lines[start..] {
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
        for ev in &parsed {
            println!("{}", serde_json::to_string(ev).unwrap_or_default());
        }
        if parsed.is_empty() {
            println!("(no matching events)");
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
        let total_all = lines.len();
        format!(" (showing {} of {} events)", total, total_all)
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
        ts[..19].to_string()
    } else {
        ts.to_string()
    }
}
