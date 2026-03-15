use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use desktop_core::{
    error::AppError,
    protocol::{RequestEnvelope, ResponseEnvelope},
};
use serde_json::{Value, json};

pub struct RecorderSession {
    frames_dir: PathBuf,
    trace_file: Mutex<File>,
}

static RECORDER: OnceLock<RecorderSession> = OnceLock::new();

pub fn record_command(
    request: &RequestEnvelope,
    response: &ResponseEnvelope,
) -> Result<(), AppError> {
    let recorder = init_session()?;
    let trace = build_trace_event(request, response, &recorder.frames_dir)?;
    let mut file = recorder
        .trace_file
        .lock()
        .map_err(|_| AppError::internal("recorder trace lock poisoned"))?;
    let line = serde_json::to_string(&trace)
        .map_err(|err| AppError::internal(format!("failed to encode trace event: {err}")))?;
    writeln!(file, "{line}").map_err(|err| {
        AppError::backend_unavailable(format!("failed to append trace event: {err}"))
    })?;
    Ok(())
}

fn init_session() -> Result<&'static RecorderSession, AppError> {
    if let Some(existing) = RECORDER.get() {
        return Ok(existing);
    }

    let session = {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let base_dir = std::env::var("DESKTOPCTL_RECORD_BASE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp/desktopctl-recordings"));
        let session_dir = base_dir.join(format!("session-{ts}-{}", std::process::id()));
        let frames_dir = session_dir.join("frames");
        fs::create_dir_all(&frames_dir).map_err(|err| {
            AppError::backend_unavailable(format!("failed to create recorder directories: {err}"))
        })?;
        let trace_path = session_dir.join("trace.jsonl");
        let trace_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trace_path)
            .map_err(|err| {
                AppError::backend_unavailable(format!("failed to open recorder trace file: {err}"))
            })?;
        RecorderSession {
            frames_dir,
            trace_file: Mutex::new(trace_file),
        }
    };

    if RECORDER.set(session).is_err() {
        // Another thread initialized it first; use the already-initialized one.
    }
    RECORDER
        .get()
        .ok_or_else(|| AppError::internal("failed to initialize recorder session"))
}

fn build_trace_event(
    request: &RequestEnvelope,
    response: &ResponseEnvelope,
    frames_dir: &Path,
) -> Result<Value, AppError> {
    let mut snapshot_id: Option<u64> = None;
    let mut event_ids: Vec<u64> = Vec::new();
    let mut frame_refs: Vec<String> = Vec::new();
    let result = match response {
        ResponseEnvelope::Success(success) => {
            snapshot_id = success.result.get("snapshot_id").and_then(|v| v.as_u64());
            if let Some(ids) = success.result.get("event_ids").and_then(|v| v.as_array()) {
                event_ids.extend(ids.iter().filter_map(|id| id.as_u64()));
            }
            if let Some(path) = success.result.get("path").and_then(|v| v.as_str()) {
                if let Some(saved_name) = copy_frame_if_present(path, frames_dir)? {
                    frame_refs.push(saved_name);
                }
            }
            success.result.clone()
        }
        ResponseEnvelope::Error(error) => json!({ "error": error.error }),
    };

    Ok(json!({
        "timestamp": now_timestamp_string(),
        "request_id": request.request_id,
        "command": request.command.name(),
        "snapshot_id": snapshot_id,
        "event_ids": event_ids,
        "result": result,
        "frames": frame_refs
    }))
}

fn copy_frame_if_present(path: &str, frames_dir: &Path) -> Result<Option<String>, AppError> {
    let source = PathBuf::from(path);
    if !source.exists() {
        return Ok(None);
    }
    let extension = source
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("png");
    let name = format!(
        "frame-{}.{}",
        now_timestamp_string().replace(':', "-"),
        extension
    );
    let destination = frames_dir.join(&name);
    fs::copy(&source, &destination).map_err(|err| {
        AppError::backend_unavailable(format!(
            "failed to copy frame {} into recorder dir: {err}",
            source.display()
        ))
    })?;
    Ok(Some(name))
}

fn now_timestamp_string() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    ms.to_string()
}

#[cfg(test)]
mod tests {
    use super::record_command;
    use desktop_core::protocol::{Command, RequestEnvelope, ResponseEnvelope};
    use serde_json::json;
    use std::{fs, path::PathBuf};

    #[test]
    fn recorder_writes_trace_and_frames_dir() {
        let base = PathBuf::from(format!("/tmp/desktopctl-rec-test-{}", std::process::id()));
        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        unsafe {
            std::env::set_var("DESKTOPCTL_RECORD_BASE", &base);
        }
        let request = RequestEnvelope::new("t1".to_string(), Command::Ping);
        let response = ResponseEnvelope::success("t1", json!({ "message": "pong" }));
        record_command(&request, &response).expect("record trace");
        let mut sessions = fs::read_dir(&base)
            .expect("read base dir")
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        sessions.sort();
        let session = sessions
            .last()
            .cloned()
            .expect("recording session directory should exist");
        assert!(session.join("trace.jsonl").exists());
        assert!(session.join("frames").exists());
    }
}
