use std::{
    fs::OpenOptions,
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};

const DEFAULT_TRACE_PATH: &str = "/tmp/desktopctld.trace.log";

pub fn log(message: impl AsRef<str>) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let pid = std::process::id();
    let tid = format!("{:?}", std::thread::current().id());
    let line = format!("{ts} pid={pid} tid={tid} {}\n", message.as_ref());

    let path = std::env::var("DESKTOPCTL_TRACE_PATH")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_TRACE_PATH.to_string());

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}
