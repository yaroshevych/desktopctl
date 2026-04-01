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

struct ActiveSession {
    session_dir: PathBuf,
    frames_dir: PathBuf,
    trace_file: File,
    started_at_ms: u128,
    stop_at_ms: Option<u128>,
    events_written: u64,
    frames_written: u64,
}

#[derive(Default)]
struct RecorderState {
    active: Option<ActiveSession>,
}

static RECORDER: OnceLock<Mutex<RecorderState>> = OnceLock::new();

pub fn start_recording(duration_ms: u64) -> Result<Value, AppError> {
    const MAX_REPLAY_DURATION_MS: u64 = 30 * 60 * 1000;
    if duration_ms == 0 {
        return Err(AppError::invalid_argument("duration_ms must be > 0"));
    }
    if duration_ms > MAX_REPLAY_DURATION_MS {
        return Err(AppError::invalid_argument(format!(
            "duration_ms exceeds max of {MAX_REPLAY_DURATION_MS}"
        )));
    }
    let mut state = recorder_state()
        .lock()
        .map_err(|_| AppError::internal("recorder state lock poisoned"))?;
    enforce_auto_stop(&mut state);
    if state.active.is_some() {
        return Err(AppError::invalid_argument(
            "replay recording is already active; use `desktopctl replay record --stop` first",
        ));
    }

    let now_ms = now_millis();
    let base_dir = std::env::var("DESKTOPCTL_RECORD_BASE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/desktopctl-recordings"));
    let session_dir = base_dir.join(format!("session-{now_ms}-{}", std::process::id()));
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

    let stop_at_ms = now_ms + duration_ms as u128;
    state.active = Some(ActiveSession {
        session_dir: session_dir.clone(),
        frames_dir,
        trace_file,
        started_at_ms: now_ms,
        stop_at_ms: Some(stop_at_ms),
        events_written: 0,
        frames_written: 0,
    });

    Ok(json!({
        "recording": true,
        "session_dir": session_dir,
        "duration_ms": duration_ms,
        "stop_at_ms": stop_at_ms.to_string()
    }))
}

pub fn stop_recording() -> Result<Value, AppError> {
    let mut state = recorder_state()
        .lock()
        .map_err(|_| AppError::internal("recorder state lock poisoned"))?;
    enforce_auto_stop(&mut state);
    let Some(session) = state.active.take() else {
        return Ok(json!({
            "recording": false,
            "stopped": false
        }));
    };
    Ok(finalize_session(session, false))
}

pub fn record_command(
    request: &RequestEnvelope,
    response: &ResponseEnvelope,
) -> Result<(), AppError> {
    let mut state = recorder_state()
        .lock()
        .map_err(|_| AppError::internal("recorder state lock poisoned"))?;
    enforce_auto_stop(&mut state);
    let Some(session) = state.active.as_mut() else {
        return Ok(());
    };

    let (trace, frame_count) = build_trace_event(request, response, &session.frames_dir)?;
    let line = serde_json::to_string(&trace)
        .map_err(|err| AppError::internal(format!("failed to encode trace event: {err}")))?;
    writeln!(session.trace_file, "{line}").map_err(|err| {
        AppError::backend_unavailable(format!("failed to append trace event: {err}"))
    })?;
    session.events_written += 1;
    session.frames_written += frame_count as u64;
    Ok(())
}

fn recorder_state() -> &'static Mutex<RecorderState> {
    RECORDER.get_or_init(|| Mutex::new(RecorderState::default()))
}

fn enforce_auto_stop(state: &mut RecorderState) {
    let should_stop = state
        .active
        .as_ref()
        .and_then(|session| session.stop_at_ms)
        .map(|deadline| now_millis() >= deadline)
        .unwrap_or(false);
    if should_stop {
        let _ = state.active.take();
    }
}

fn finalize_session(session: ActiveSession, auto_stopped: bool) -> Value {
    json!({
        "recording": false,
        "stopped": true,
        "auto_stopped": auto_stopped,
        "session_dir": session.session_dir,
        "started_at_ms": session.started_at_ms.to_string(),
        "events_written": session.events_written,
        "frames_written": session.frames_written
    })
}

fn build_trace_event(
    request: &RequestEnvelope,
    response: &ResponseEnvelope,
    frames_dir: &Path,
) -> Result<(Value, usize), AppError> {
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

    let frame_count = frame_refs.len();
    Ok((
        json!({
            "timestamp": now_timestamp_string(),
            "request_id": request.request_id,
            "command": request.command.name(),
            "snapshot_id": snapshot_id,
            "event_ids": event_ids,
            "result": result,
            "frames": frame_refs
        }),
        frame_count,
    ))
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

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn now_timestamp_string() -> String {
    now_millis().to_string()
}

#[cfg(test)]
mod tests {
    use super::{record_command, start_recording, stop_recording};
    use desktop_core::protocol::{Command, RequestEnvelope, ResponseEnvelope};
    use serde_json::json;
    use std::{fs, path::PathBuf};

    #[test]
    fn recorder_writes_trace_when_active() {
        let base = PathBuf::from(format!("/tmp/desktopctl-rec-test-{}", std::process::id()));
        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        unsafe {
            std::env::set_var("DESKTOPCTL_RECORD_BASE", &base);
        }

        let _ = stop_recording();
        start_recording(3_000).expect("start recording");

        let request = RequestEnvelope::new("t1".to_string(), Command::Ping);
        let response = ResponseEnvelope::success("t1", json!({ "message": "pong" }));
        record_command(&request, &response).expect("record trace");
        let _ = stop_recording().expect("stop recording");

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
