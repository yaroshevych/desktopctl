#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::{
    env,
    io::{Read, Write},
    path::PathBuf,
};
#[cfg(windows)]
use std::{
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    time::Duration,
};

use crate::{
    error::AppError,
    protocol::{RequestEnvelope, ResponseEnvelope},
};

const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;
#[cfg(unix)]
const SOCKET_DIR_NAME: &str = "desktopctl";
#[cfg(unix)]
const SOCKET_FILE_NAME: &str = "desktopctl.sock";
#[cfg(windows)]
const DEFAULT_WINDOWS_ADDR: &str = "127.0.0.1:42737";
#[cfg(windows)]
const WINDOWS_TOKEN_FILE_NAME: &str = "desktopctl-ipc-token";

#[cfg(unix)]
fn legacy_socket_path() -> PathBuf {
    PathBuf::from("/tmp/desktopctl.sock")
}

#[cfg(unix)]
pub fn socket_path() -> PathBuf {
    if let Some(path) = env::var_os("DESKTOPCTL_SOCKET_PATH") {
        return PathBuf::from(path);
    }
    env::temp_dir().join(SOCKET_DIR_NAME).join(SOCKET_FILE_NAME)
}

#[cfg(windows)]
pub fn socket_addr() -> String {
    env::var("DESKTOPCTL_SOCKET_ADDR").unwrap_or_else(|_| DEFAULT_WINDOWS_ADDR.to_string())
}

#[cfg(windows)]
pub fn windows_ipc_token_path() -> PathBuf {
    if let Some(path) = env::var_os("DESKTOPCTL_IPC_TOKEN_PATH") {
        return PathBuf::from(path);
    }
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("DesktopCtl")
            .join(WINDOWS_TOKEN_FILE_NAME);
    }
    env::temp_dir()
        .join("desktopctl")
        .join(WINDOWS_TOKEN_FILE_NAME)
}

pub fn send_request(request: &RequestEnvelope) -> Result<ResponseEnvelope, AppError> {
    #[cfg(unix)]
    {
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
        return read_framed_json(&mut stream);
    }

    #[cfg(windows)]
    {
        let addr = socket_addr();
        let mut stream = TcpStream::connect(&addr).map_err(|err| {
            AppError::daemon_not_running(format!(
                "failed to connect to {addr}: {err}. is desktopctld running?"
            ))
        })?;
        let token = load_windows_ipc_token()?;
        write_windows_auth_line(&mut stream, &token)?;
        write_framed_json(&mut stream, request)?;
        return read_framed_json(&mut stream);
    }

    #[allow(unreachable_code)]
    Err(AppError::backend_unavailable(format!(
        "unsupported platform: {}",
        std::env::consts::OS
    )))
}

pub fn socket_exists() -> bool {
    #[cfg(unix)]
    {
        let path = socket_path();
        return path.exists() || (path != legacy_socket_path() && legacy_socket_path().exists());
    }

    #[cfg(windows)]
    {
        let addr = match resolve_socket_addr(&socket_addr()) {
            Ok(addr) => addr,
            Err(_) => return false,
        };
        return TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok();
    }

    #[allow(unreachable_code)]
    false
}

#[cfg(windows)]
fn resolve_socket_addr(addr: &str) -> Result<SocketAddr, AppError> {
    let mut candidates = addr.to_socket_addrs().map_err(|err| {
        AppError::invalid_argument(format!(
            "invalid DESKTOPCTL_SOCKET_ADDR value '{addr}': {err}"
        ))
    })?;
    candidates.next().ok_or_else(|| {
        AppError::invalid_argument(format!(
            "invalid DESKTOPCTL_SOCKET_ADDR value '{addr}': no socket addresses found"
        ))
    })
}

#[cfg(windows)]
fn load_windows_ipc_token() -> Result<String, AppError> {
    let path = windows_ipc_token_path();
    let raw = std::fs::read_to_string(&path).map_err(|err| {
        AppError::daemon_not_running(format!(
            "failed to read DesktopCtl IPC token at {}: {err}. is desktopctld running?",
            path.display()
        ))
    })?;
    let token = raw.trim().to_string();
    if token.is_empty() {
        return Err(AppError::daemon_not_running(format!(
            "DesktopCtl IPC token file is empty at {}. restart desktopctld",
            path.display()
        )));
    }
    Ok(token)
}

#[cfg(windows)]
fn write_windows_auth_line(stream: &mut TcpStream, token: &str) -> Result<(), AppError> {
    stream
        .write_all(format!("AUTH {token}\n").as_bytes())
        .map_err(|err| {
            AppError::backend_unavailable(format!("failed to write IPC auth prelude: {err}"))
        })?;
    stream.flush().map_err(|err| {
        AppError::backend_unavailable(format!("failed to flush IPC auth prelude: {err}"))
    })
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
