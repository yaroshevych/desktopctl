use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
};

use crate::{
    error::AppError,
    protocol::{Request, Response},
};

pub fn socket_path() -> PathBuf {
    PathBuf::from("/tmp/desktopctl.sock")
}

pub fn send_request(request: &Request) -> Result<Response, AppError> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).map_err(|err| {
        AppError::Ipc(format!(
            "failed to connect to {}: {}. is DesktopCtl.app running?",
            path.display(),
            err
        ))
    })?;

    let payload = serde_json::to_vec(request)
        .map_err(|err| AppError::Ipc(format!("encode failed: {err}")))?;
    stream
        .write_all(&payload)
        .map_err(|err| AppError::Ipc(format!("send failed: {err}")))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|err| AppError::Ipc(format!("shutdown failed: {err}")))?;

    let mut response_buf = Vec::new();
    stream
        .read_to_end(&mut response_buf)
        .map_err(|err| AppError::Ipc(format!("read failed: {err}")))?;

    serde_json::from_slice(&response_buf)
        .map_err(|err| AppError::Ipc(format!("invalid response: {err}")))
}

pub fn socket_exists() -> bool {
    socket_path().exists()
}
