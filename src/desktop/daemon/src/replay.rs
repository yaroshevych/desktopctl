use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use desktop_core::error::AppError;
use serde_json::{Value, json};

pub fn load_session(session_dir: &Path) -> Result<Value, AppError> {
    let trace_path = session_dir.join("trace.jsonl");
    let file = File::open(&trace_path).map_err(|err| {
        AppError::invalid_argument(format!(
            "failed to open replay trace {}: {err}",
            trace_path.display()
        ))
    })?;
    let reader = BufReader::new(file);
    let mut events_loaded = 0_u64;
    let mut missing_frames = Vec::<String>::new();

    for line in reader.lines() {
        let line = line.map_err(|err| {
            AppError::backend_unavailable(format!("failed reading replay trace: {err}"))
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).map_err(|err| {
            AppError::invalid_argument(format!("invalid replay event JSON: {err}"))
        })?;
        events_loaded += 1;

        if let Some(frames) = value.get("frames").and_then(|v| v.as_array()) {
            for frame in frames {
                if let Some(name) = frame.as_str() {
                    let frame_path = session_dir.join("frames").join(name);
                    if !frame_path.exists() {
                        missing_frames.push(name.to_string());
                    }
                }
            }
        }
    }

    Ok(json!({
        "session_dir": session_dir,
        "events_loaded": events_loaded,
        "missing_frames": missing_frames
    }))
}

pub fn parse_session_dir(input: &str) -> Result<PathBuf, AppError> {
    let path = PathBuf::from(input);
    if !path.exists() {
        return Err(AppError::invalid_argument(format!(
            "replay session directory does not exist: {}",
            path.display()
        )));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::load_session;
    use std::{fs, path::PathBuf};

    #[test]
    fn replay_loads_trace_events() {
        let dir = PathBuf::from(format!(
            "/tmp/desktopctl-replay-test-{}",
            std::process::id()
        ));
        if dir.exists() {
            let _ = fs::remove_dir_all(&dir);
        }
        fs::create_dir_all(dir.join("frames")).expect("create replay fixture dir");
        fs::write(
            dir.join("trace.jsonl"),
            "{\"timestamp\":\"1\",\"command\":\"ping\",\"snapshot_id\":null,\"event_ids\":[],\"result\":{\"message\":\"pong\"},\"frames\":[]}\n",
        )
        .expect("write trace");
        let result = load_session(&dir).expect("load session");
        assert_eq!(result["events_loaded"], 1);
    }
}
