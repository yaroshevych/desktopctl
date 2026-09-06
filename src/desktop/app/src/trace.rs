use std::{
    fs::OpenOptions,
    io::Write,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

/// Correlates one launcher request across the UI, window capture, Pi, and
/// completion notification paths. The wall-clock timestamp on each trace line
/// is useful across processes; this monotonic elapsed value is useful for
/// measuring one end-to-end request without clock adjustments getting involved.
#[derive(Debug)]
pub(crate) struct E2eTiming {
    id: u64,
    started: Instant,
}

static NEXT_E2E_ID: AtomicU64 = AtomicU64::new(1);
static ACTIVE_LAUNCHER_TIMING: OnceLock<Mutex<Option<Arc<E2eTiming>>>> = OnceLock::new();

impl E2eTiming {
    pub(crate) fn new(kind: &str) -> Arc<Self> {
        let timing = Arc::new(Self {
            id: NEXT_E2E_ID.fetch_add(1, Ordering::Relaxed),
            started: Instant::now(),
        });
        timing.mark("start", format!("kind={kind}"));
        timing
    }

    pub(crate) fn mark(&self, event: &str, details: impl AsRef<str>) {
        log(format!(
            "e2e id={} elapsed_ms={} event={} {}",
            self.id,
            self.started.elapsed().as_millis(),
            event,
            details.as_ref()
        ));
    }
}

pub(crate) fn begin_launcher_timing() -> Arc<E2eTiming> {
    let timing = E2eTiming::new("launcher");
    let active = ACTIVE_LAUNCHER_TIMING.get_or_init(|| Mutex::new(None));
    *active
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::clone(&timing));
    timing
}

pub(crate) fn current_launcher_timing() -> Option<Arc<E2eTiming>> {
    ACTIVE_LAUNCHER_TIMING
        .get()
        .and_then(|active| active.lock().ok().and_then(|timing| timing.clone()))
}

pub(crate) fn take_launcher_timing() -> Option<Arc<E2eTiming>> {
    ACTIVE_LAUNCHER_TIMING
        .get()
        .and_then(|active| active.lock().ok().and_then(|mut timing| timing.take()))
}

fn write_line(message: &str) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or(0);
    let line = format!(
        "{timestamp} pid={} tid={:?} {message}\n",
        std::process::id(),
        std::thread::current().id(),
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

pub fn log(message: impl AsRef<str>) {
    if !enabled() {
        return;
    }
    write_line(message.as_ref());
}

pub(crate) fn enabled() -> bool {
    std::env::var("DESKTOPCTL_TRACE")
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
        || std::env::var("DESKTOPCTL_TRACE_PATH")
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
}

/// Opt-in diagnostics for the launcher-to-Pi DesktopCtl context handoff.
///
/// Enable with `DESKTOPCTL_AGENT_CONTEXT_TRACE=1`; output uses
/// `DESKTOPCTL_TRACE_PATH` when provided, otherwise the normal DesktopCtl log.
pub fn agent_context(message: impl AsRef<str>) {
    let enabled = std::env::var("DESKTOPCTL_AGENT_CONTEXT_TRACE")
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"));
    if enabled {
        write_line(&format!("agent_context: {}", message.as_ref()));
    }
}
