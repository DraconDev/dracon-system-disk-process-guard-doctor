//! Tests for events.rs (event types and severity classification)
//!
//! These tests verify the event components after extraction from main.rs.

#[test]
fn event_severity_debug() {
    use crate::EventSeverity;
    let sev = EventSeverity::Warn;
    let debug = format!("{:?}", sev);
    assert!(debug.contains("Warn"));
}

#[test]
fn dracon_event_debug() {
    use crate::{DraconEvent, EventSeverity};
    let event = DraconEvent {
        domain: "guard".to_string(),
        severity: EventSeverity::Warn,
        path: "/tmp/test".to_string(),
        message: "test warning".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    let debug = format!("{:?}", event);
    assert!(debug.contains("guard"));
    assert!(debug.contains("Warn"));
    assert!(debug.contains("test warning"));
}

#[test]
fn dracon_event_all_severity_levels() {
    use crate::EventSeverity;
    // Just verify all variants exist
    let _ = EventSeverity::Info;
    let _ = EventSeverity::Warn;
    let _ = EventSeverity::Error;
}

#[test]
fn dracon_event_with_empty_path() {
    use crate::{DraconEvent, EventSeverity};
    let event = DraconEvent {
        domain: "system".to_string(),
        severity: EventSeverity::Info,
        path: String::new(),
        message: "no path event".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    assert!(event.path.is_empty());
    assert_eq!(event.message, "no path event");
}

#[test]
fn dracon_event_via_new_constructor() {
    use crate::{DraconEvent, EventSeverity};
    let event = DraconEvent::new("guard", EventSeverity::Error, "/tmp/path", "error occurred");
    assert_eq!(event.domain, "guard");
    assert_eq!(event.message, "error occurred");
    assert!(!event.timestamp.is_empty());
}

#[test]
fn emit_event_does_not_panic() {
    use crate::{DraconEvent, EventSeverity};
    let event = DraconEvent::new("test", EventSeverity::Info, "/tmp", "test event");
    crate::emit_event(&event);
}

#[test]
fn shorten_event_time_relative() {
    use crate::shorten_event_time;
    let now = chrono::Utc::now();
    let ts_30s = (now - chrono::Duration::seconds(30)).to_rfc3339();
    assert_eq!(shorten_event_time(&ts_30s), "30s");

    let ts_5m = (now - chrono::Duration::minutes(5)).to_rfc3339();
    assert_eq!(shorten_event_time(&ts_5m), "5m");

    let ts_2h = (now - chrono::Duration::hours(2)).to_rfc3339();
    assert_eq!(shorten_event_time(&ts_2h), "2h");

    let ts_3d = (now - chrono::Duration::days(3)).to_rfc3339();
    assert_eq!(shorten_event_time(&ts_3d), "3d");

    let ts_60d = (now - chrono::Duration::days(60)).to_rfc3339();
    assert_eq!(shorten_event_time(&ts_60d), "2mo");
}

#[test]
fn shorten_event_time_non_rfc3339() {
    use crate::shorten_event_time;
    assert_eq!(shorten_event_time("not-a-date"), "not-a-date");
    let truncated = "2026-05-15T17:22:57";
    assert_eq!(shorten_event_time(truncated), "2026-05-15T17:22:57");
}
