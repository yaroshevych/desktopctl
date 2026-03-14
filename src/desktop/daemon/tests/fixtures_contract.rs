use std::{fs, path::PathBuf};

#[test]
fn snapshot_fixture_corpus_is_present() {
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/snapshots");
    let entries = fs::read_dir(&fixtures_dir).expect("snapshot fixtures directory should exist");
    let mut json_files = Vec::new();
    for entry in entries {
        let entry = entry.expect("read fixture entry");
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            json_files.push(path);
        }
    }
    assert!(
        json_files.len() >= 10,
        "expected at least 10 snapshot fixture files, got {}",
        json_files.len()
    );

    for file in json_files {
        let raw = fs::read_to_string(&file).expect("read fixture JSON");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON fixture");
        assert!(
            value.get("snapshot_id").is_some(),
            "snapshot_id missing in {}",
            file.display()
        );
        assert!(
            value.get("timestamp").is_some(),
            "timestamp missing in {}",
            file.display()
        );
        assert!(
            value.get("display").is_some(),
            "display missing in {}",
            file.display()
        );
        assert!(
            value.get("texts").is_some(),
            "texts missing in {}",
            file.display()
        );
    }
}

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
