use std::{
    fs::OpenOptions,
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn log(message: impl AsRef<str>) {
    let enabled = std::env::var("DESKTOPCTL_TRACE")
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
        || std::env::var("DESKTOPCTL_TRACE_PATH")
            .ok()
            .is_some_and(|value| !value.trim().is_empty());
    if !enabled {
        return;
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or(0);
    let line = format!(
        "{timestamp} pid={} tid={:?} {}\n",
        std::process::id(),
        std::thread::current().id(),
        message.as_ref()
    );
    let path = std::env::var("DESKTOPCTL_TRACE_PATH")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let paths = desktop_core::paths::AppPaths::resolve().ok()?;
            paths.ensure_logs_dir().ok()?;
            Some(paths.daemon_log_file())
        });
    if let Some(path) = path {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(line.as_bytes());
        }
    }
}
