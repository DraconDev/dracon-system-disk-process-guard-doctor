use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_home_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "dracon-system-f68-home-{}-{nanos}",
        std::process::id()
    ))
}

fn run_events(home: &Path, extra_args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dracon-system"))
        .env("HOME", home)
        .args(["events", "--json"])
        .args(extra_args)
        .output()
        .expect("run events")
}

fn parse_jsonl(output: Output) -> Vec<serde_json::Value> {
    assert!(
        output.status.success(),
        "events --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout)
        .expect("events --json should emit UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("every JSONL line should be valid JSON"))
        .collect()
}

#[test]
fn events_json_with_missing_log_emits_empty_jsonl() {
    let home = unique_home_path();
    fs::create_dir_all(&home).expect("create isolated home");

    let events = parse_jsonl(run_events(&home, &[]));
    assert!(events.is_empty());

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn events_json_with_empty_log_emits_empty_jsonl() {
    let home = unique_home_path();
    let event_dir = home.join(".dracon");
    fs::create_dir_all(&event_dir).expect("create event directory");
    fs::write(event_dir.join("events.jsonl"), "").expect("write empty event log");

    let events = parse_jsonl(run_events(&home, &[]));
    assert!(events.is_empty());

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn events_json_includes_rotated_segment_before_active_segment() {
    let home = unique_home_path();
    let event_dir = home.join(".dracon");
    fs::create_dir_all(&event_dir).expect("create event directory");
    fs::write(
        event_dir.join("events.jsonl.1"),
        r#"{"domain":"system","severity":"Info","path":"/old","message":"old event","timestamp":"2026-01-01T00:00:00Z"}
"#,
    )
    .expect("write rotated event log");
    fs::write(
        event_dir.join("events.jsonl"),
        r#"{"domain":"system","severity":"Warn","path":"/new","message":"new event","timestamp":"2026-01-01T00:00:01Z"}
"#,
    )
    .expect("write active event log");

    let events = parse_jsonl(run_events(&home, &[]));
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["message"], "old event");
    assert_eq!(events[1]["message"], "new event");

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn events_json_with_filter_matching_nothing_emits_empty_jsonl() {
    let home = unique_home_path();
    let event_dir = home.join(".dracon");
    fs::create_dir_all(&event_dir).expect("create event directory");
    fs::write(
        event_dir.join("events.jsonl"),
        r#"{"domain":"system","severity":"Info","path":"/tmp","message":"kept event","timestamp":"2026-01-01T00:00:00Z"}
"#,
    )
    .expect("write event log");

    let events = parse_jsonl(run_events(&home, &["--source", "warden"]));
    assert!(events.is_empty());

    let _ = fs::remove_dir_all(&home);
}
