use std::{fs, path::PathBuf};

#[test]
fn replay_fixture_trace_has_required_fields() {
    let trace_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/replay/session-a/trace.jsonl");
    let trace = fs::read_to_string(&trace_path).expect("read replay fixture trace");
    let mut count = 0;
    for line in trace.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let event: serde_json::Value = serde_json::from_str(line).expect("valid trace line JSON");
        assert!(event.get("timestamp").is_some());
        assert!(event.get("command").is_some());
        assert!(event.get("result").is_some());
        count += 1;
    }
    assert!(count >= 2, "expected at least two replay events");
}

#[test]
fn replay_fixture_trace_timestamps_are_non_decreasing() {
    let trace_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/replay/session-a/trace.jsonl");
    let trace = fs::read_to_string(&trace_path).expect("read replay fixture trace");
    let mut prev: Option<u64> = None;
    for line in trace.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let event: serde_json::Value = serde_json::from_str(line).expect("valid trace line JSON");
        let ts: u64 = event
            .get("timestamp")
            .and_then(|t| t.as_str())
            .expect("timestamp is a string")
            .parse()
            .expect("timestamp parses as integer millis");
        if let Some(previous) = prev {
            assert!(
                ts >= previous,
                "replay events must be ordered by timestamp: {ts} < {previous}"
            );
        }
        prev = Some(ts);
    }
}
