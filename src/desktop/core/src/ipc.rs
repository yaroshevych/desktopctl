use std::{
    env,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
};

use crate::{
    error::AppError,
    protocol::{RequestEnvelope, ResponseEnvelope},
};

const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;
const SOCKET_DIR_NAME: &str = "desktopctl";
const SOCKET_FILE_NAME: &str = "desktopctl.sock";

fn legacy_socket_path() -> PathBuf {
    PathBuf::from("/tmp/desktopctl.sock")
}

pub fn socket_path() -> PathBuf {
    if let Some(path) = env::var_os("DESKTOPCTL_SOCKET_PATH") {
        return PathBuf::from(path);
    }
    env::temp_dir().join(SOCKET_DIR_NAME).join(SOCKET_FILE_NAME)
}

pub fn send_request(request: &RequestEnvelope) -> Result<ResponseEnvelope, AppError> {
    let path = socket_path();
    let stream = UnixStream::connect(&path).or_else(|primary_err| {
        let legacy = legacy_socket_path();
        if legacy == path {
            return Err(primary_err);
        }
        UnixStream::connect(&legacy).map_err(|_| primary_err)
    });
    let mut stream = stream.map_err(|err| {
        AppError::daemon_not_running(format!(
            "failed to connect to {}: {}. is DesktopCtl.app running?",
            path.display(),
            err
        ))
    })?;

    write_framed_json(&mut stream, request)?;
    read_framed_json(&mut stream)
}

pub fn socket_exists() -> bool {
    let path = socket_path();
    path.exists() || (path != legacy_socket_path() && legacy_socket_path().exists())
}

pub fn write_framed_json<W: Write, T: serde::Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), AppError> {
    let payload = serde_json::to_vec(value)
        .map_err(|err| AppError::internal(format!("failed to encode JSON: {err}")))?;
    if payload.len() > MAX_FRAME_SIZE {
        return Err(AppError::invalid_argument(format!(
            "frame too large: {} bytes",
            payload.len()
        )));
    }

    let len = payload.len() as u32;
    writer.write_all(&len.to_be_bytes()).map_err(|err| {
        AppError::backend_unavailable(format!("failed to write frame header: {err}"))
    })?;
    writer.write_all(&payload).map_err(|err| {
        AppError::backend_unavailable(format!("failed to write frame body: {err}"))
    })?;
    writer.flush().map_err(|err| {
        AppError::backend_unavailable(format!("failed to flush framed payload: {err}"))
    })?;
    Ok(())
}

pub fn read_framed_json<R: Read, T: serde::de::DeserializeOwned>(
    reader: &mut R,
) -> Result<T, AppError> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).map_err(|err| {
        AppError::backend_unavailable(format!("failed to read frame header: {err}"))
    })?;
    let frame_len = u32::from_be_bytes(header) as usize;
    if frame_len == 0 {
        return Err(AppError::invalid_argument("received empty frame"));
    }
    if frame_len > MAX_FRAME_SIZE {
        return Err(AppError::invalid_argument(format!(
            "frame too large: {frame_len} bytes"
        )));
    }

    let mut payload = vec![0_u8; frame_len];
    reader.read_exact(&mut payload).map_err(|err| {
        AppError::backend_unavailable(format!("failed to read frame body: {err}"))
    })?;
    serde_json::from_slice::<T>(&payload)
        .map_err(|err| AppError::invalid_argument(format!("invalid JSON payload: {err}")))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use crate::protocol::{Command, RequestEnvelope, ResponseEnvelope};

    use super::{read_framed_json, write_framed_json};

    #[test]
    fn frame_roundtrip() {
        let req = RequestEnvelope::new("r1".to_string(), Command::Ping);
        let mut buf = Vec::new();
        write_framed_json(&mut buf, &req).expect("encode");

        let mut cursor = Cursor::new(buf);
        let decoded: RequestEnvelope = read_framed_json(&mut cursor).expect("decode");
        assert_eq!(decoded.request_id, "r1");
        assert!(matches!(decoded.command, Command::Ping));
    }

    #[test]
    fn malformed_frame_rejected() {
        let mut cursor = Cursor::new(vec![0, 0, 0, 2, b'{']);
        let err = read_framed_json::<_, ResponseEnvelope>(&mut cursor).expect_err("bad frame");
        assert!(err.message.contains("frame body"));
    }

    #[test]
    fn invalid_json_rejected() {
        let mut payload = vec![0, 0, 0, 5];
        payload.extend_from_slice(b"abcde");
        let mut cursor = Cursor::new(payload);
        let err = read_framed_json::<_, ResponseEnvelope>(&mut cursor).expect_err("bad json");
        assert!(err.message.contains("invalid JSON"));
    }
}
