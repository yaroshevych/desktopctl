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
