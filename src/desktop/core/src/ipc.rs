#[cfg(windows)]
use std::fs;
#[cfg(any(unix, windows))]
use std::path::PathBuf;
use std::{
    env,
    io::{Read, Write},
};

#[cfg(unix)]
use interprocess::local_socket::{GenericFilePath, ToFsName};
#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, ToNsName};
use interprocess::local_socket::{Stream, prelude::*};

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
const DEFAULT_WINDOWS_PIPE_NAME: &str = "desktopctl";
#[cfg(windows)]
const WINDOWS_AUTH_DIR_NAME: &str = "desktopctl";
#[cfg(windows)]
const WINDOWS_AUTH_FILE_NAME: &str = "ipc-token";
#[cfg(windows)]
const AUTH_PREFIX: &str = "AUTH ";

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
pub fn pipe_name() -> String {
    if let Ok(name) = env::var("DESKTOPCTL_PIPE_NAME") {
        if !name.trim().is_empty() {
            return name;
        }
    }
    if let Some(sid) = current_user_sid_component() {
        return format!("{DEFAULT_WINDOWS_PIPE_NAME}-sid-{sid}");
    }
    let user = env::var("USERNAME")
        .ok()
        .map(|name| sanitize_pipe_component(&name))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "user".to_string());
    format!("{DEFAULT_WINDOWS_PIPE_NAME}-{user}")
}

pub fn send_request(request: &RequestEnvelope) -> Result<ResponseEnvelope, AppError> {
    #[cfg(unix)]
    {
        let path = socket_path();
        let connect = || -> std::io::Result<Stream> {
            let name = path.as_os_str().to_fs_name::<GenericFilePath>()?;
            Stream::connect(name)
        };

        let mut stream = connect()
            .or_else(|primary_err| {
                let legacy = legacy_socket_path();
                if legacy == path {
                    return Err(primary_err);
                }
                let legacy_name = legacy.as_os_str().to_fs_name::<GenericFilePath>()?;
                Stream::connect(legacy_name).map_err(|_| primary_err)
            })
            .map_err(|err| {
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
        let name_raw = pipe_name();
        let name = name_raw
            .clone()
            .to_ns_name::<GenericNamespaced>()
            .map_err(|err| {
                AppError::invalid_argument(format!(
                    "invalid DESKTOPCTL_PIPE_NAME '{name_raw}': {err}"
                ))
            })?;

        let mut stream = Stream::connect(name).map_err(|err| {
            AppError::daemon_not_running(format!(
                "failed to connect to named pipe '{name_raw}': {err}. is desktopctld running?"
            ))
        })?;

        send_windows_client_auth(&mut stream)?;
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
        let name_raw = pipe_name();
        let name = match name_raw.to_ns_name::<GenericNamespaced>() {
            Ok(name) => name,
            Err(_) => return false,
        };
        return Stream::connect(name).is_ok();
    }

    #[allow(unreachable_code)]
    false
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

#[cfg(windows)]
fn sanitize_pipe_component(input: &str) -> String {
    let mut out = String::new();
    let mut prev_sep = false;
    for ch in input.trim().chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_sep = false;
        } else if !prev_sep {
            out.push('-');
            prev_sep = true;
        }
        if out.len() >= 32 {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(windows)]
fn current_user_sid_component() -> Option<String> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, LocalFree},
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    // SAFETY: GetCurrentProcess returns a valid pseudo-handle for OpenProcessToken.
    let process: HANDLE = unsafe { GetCurrentProcess() };
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: token out pointer is valid.
    let ok = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token as *mut HANDLE) };
    if ok == 0 || token.is_null() {
        return None;
    }

    let mut needed = 0_u32;
    // SAFETY: probe call requesting required size.
    unsafe {
        let _ = GetTokenInformation(
            token,
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut needed as *mut u32,
        );
    }
    if needed == 0 {
        // SAFETY: token came from OpenProcessToken.
        unsafe { CloseHandle(token) };
        return None;
    }

    let mut buffer = vec![0_u8; needed as usize];
    // SAFETY: buffer points to writable memory sized by the prior probe.
    let info_ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr() as *mut _,
            needed,
            &mut needed as *mut u32,
        )
    };
    if info_ok == 0 {
        // SAFETY: token came from OpenProcessToken.
        unsafe { CloseHandle(token) };
        return None;
    }

    // SAFETY: TOKEN_USER is the buffer layout for TokenUser.
    let token_user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
    let mut sid_wide: *mut u16 = std::ptr::null_mut();
    // SAFETY: SID pointer is owned by token_user from GetTokenInformation.
    let sid_ok =
        unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut sid_wide as *mut *mut u16) };

    // SAFETY: token came from OpenProcessToken.
    unsafe { CloseHandle(token) };
    if sid_ok == 0 || sid_wide.is_null() {
        return None;
    }

    let mut len = 0_usize;
    // SAFETY: sid_wide is NUL-terminated UTF-16 per ConvertSidToStringSidW contract.
    unsafe {
        while *sid_wide.add(len) != 0 {
            len += 1;
        }
    }
    // SAFETY: sid_wide points to `len` UTF-16 code units.
    let sid = unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(sid_wide, len)) };
    // SAFETY: memory owned by LocalAlloc, must be freed with LocalFree.
    unsafe {
        let _ = LocalFree(sid_wide as *mut core::ffi::c_void);
    }
    let normalized = sanitize_pipe_component(&sid);
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

#[cfg(windows)]
fn windows_auth_token_path() -> PathBuf {
    if let Ok(path) = env::var("DESKTOPCTL_IPC_TOKEN_PATH") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    let local_app_data = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .map(|base| base.join("AppData").join("Local"))
        })
        .unwrap_or_else(env::temp_dir);
    local_app_data
        .join(WINDOWS_AUTH_DIR_NAME)
        .join(WINDOWS_AUTH_FILE_NAME)
}

#[cfg(windows)]
fn load_or_create_windows_auth_token() -> Result<String, AppError> {
    let path = windows_auth_token_path();
    if let Ok(existing) = fs::read_to_string(&path) {
        let token = existing.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::backend_unavailable(format!(
                "failed to create auth token directory {}: {err}",
                parent.display()
            ))
        })?;
    }
    let token = uuid::Uuid::new_v4().to_string();
    match fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(token.as_bytes()).map_err(|err| {
                AppError::backend_unavailable(format!(
                    "failed to write auth token file {}: {err}",
                    path.display()
                ))
            })?;
            Ok(token)
        }
        Err(_) => {
            let existing = fs::read_to_string(&path).map_err(|err| {
                AppError::backend_unavailable(format!(
                    "failed to read auth token file {}: {err}",
                    path.display()
                ))
            })?;
            let token = existing.trim().to_string();
            if token.is_empty() {
                return Err(AppError::backend_unavailable(format!(
                    "auth token file {} is empty",
                    path.display()
                )));
            }
            Ok(token)
        }
    }
}

#[cfg(windows)]
pub fn send_windows_client_auth<W: Write>(writer: &mut W) -> Result<(), AppError> {
    let token = load_or_create_windows_auth_token()?;
    let line = format!("{AUTH_PREFIX}{token}\n");
    writer.write_all(line.as_bytes()).map_err(|err| {
        AppError::backend_unavailable(format!("failed to write auth prelude: {err}"))
    })?;
    writer.flush().map_err(|err| {
        AppError::backend_unavailable(format!("failed to flush auth prelude: {err}"))
    })?;
    Ok(())
}

#[cfg(windows)]
pub fn expect_windows_client_auth<R: Read>(reader: &mut R) -> Result<(), AppError> {
    let expected = load_or_create_windows_auth_token()?;
    let mut line = Vec::with_capacity(96);
    let mut byte = [0_u8; 1];
    while line.len() < 1024 {
        reader.read_exact(&mut byte).map_err(|err| {
            AppError::permission_denied(format!("failed to read auth prelude: {err}"))
        })?;
        if byte[0] == b'\n' {
            break;
        }
        if byte[0] != b'\r' {
            line.push(byte[0]);
        }
    }
    if line.is_empty() {
        return Err(AppError::permission_denied("missing auth prelude"));
    }
    let line_text = String::from_utf8(line)
        .map_err(|_| AppError::permission_denied("invalid auth prelude encoding"))?;
    let provided = line_text
        .strip_prefix(AUTH_PREFIX)
        .ok_or_else(|| AppError::permission_denied("invalid auth prelude prefix"))?;
    if provided.trim() != expected {
        return Err(AppError::permission_denied("invalid IPC auth token"));
    }
    Ok(())
}
