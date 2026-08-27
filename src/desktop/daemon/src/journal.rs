use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use desktop_core::{
    error::AppError,
    protocol::{Command, ResponseEnvelope},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{daemon, trace};

const DEFAULT_INTERVAL_SECONDS: u64 = 30;
const MIN_INTERVAL_SECONDS: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_interval_seconds")]
    pub interval_seconds: u64,
    #[serde(default = "default_output_dir")]
    pub output_dir: PathBuf,
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_seconds: DEFAULT_INTERVAL_SECONDS,
            output_dir: default_output_dir(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadOutcome {
    pub config: JournalConfig,
    pub warning: Option<String>,
}

type StateHook = fn(bool);

static CURRENT_CONFIG: OnceLock<Mutex<JournalConfig>> = OnceLock::new();
static ACTIVE: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static STATE_HOOKS: OnceLock<Mutex<Vec<StateHook>>> = OnceLock::new();

fn current_config() -> &'static Mutex<JournalConfig> {
    CURRENT_CONFIG.get_or_init(|| Mutex::new(JournalConfig::default()))
}

fn state_hooks() -> &'static Mutex<Vec<StateHook>> {
    STATE_HOOKS.get_or_init(|| Mutex::new(Vec::new()))
}

fn default_interval_seconds() -> u64 {
    DEFAULT_INTERVAL_SECONDS
}

fn default_output_dir() -> PathBuf {
    desktop_core::paths::AppPaths::resolve()
        .map(|paths| paths.logs_dir().join("journal"))
        .expect("DesktopCtl home requires DESKTOPCTL_HOME or HOME")
}

#[allow(dead_code)]
pub fn register_state_hook(hook: StateHook) {
    if let Ok(mut hooks) = state_hooks().lock() {
        hooks.push(hook);
    }
}

#[allow(dead_code)]
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

pub fn current() -> JournalConfig {
    current_config()
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_else(|_| JournalConfig::default())
}

pub fn load_current_from_disk() -> LoadOutcome {
    let outcome = load_with_diagnostics();
    set_current_memory(&outcome.config);
    outcome
}

pub fn load_with_diagnostics() -> LoadOutcome {
    match crate::storage::load_journal() {
        Ok(Some(mut config)) => {
            normalize_config(&mut config);
            LoadOutcome {
                config,
                warning: None,
            }
        }
        Ok(None) => LoadOutcome {
            config: JournalConfig::default(),
            warning: None,
        },
        Err(error) => {
            let warning = format!("{error}; using defaults");
            LoadOutcome {
                config: JournalConfig::default(),
                warning: Some(warning),
            }
        }
    }
}

pub fn save(config: &JournalConfig) -> Result<(), String> {
    let mut normalized = config.clone();
    normalize_config(&mut normalized);
    crate::storage::save_journal(&normalized)
}

pub fn apply(config: JournalConfig) -> Result<(), String> {
    let mut normalized = config;
    normalize_config(&mut normalized);
    save(&normalized)?;
    set_current_memory(&normalized);
    restart_from_current();
    Ok(())
}

pub fn start_from_disk() {
    let outcome = load_current_from_disk();
    if let Some(warning) = outcome.warning {
        trace::log(format!("journal:config_warn {warning}"));
    }
    restart_from_current();
}

fn set_current_memory(config: &JournalConfig) {
    if let Ok(mut guard) = current_config().lock() {
        *guard = config.clone();
    }
}

fn normalize_config(config: &mut JournalConfig) {
    config.interval_seconds = config.interval_seconds.max(MIN_INTERVAL_SECONDS);
    if config.output_dir.as_os_str().is_empty() {
        config.output_dir = default_output_dir();
    }
}

fn restart_from_current() {
    let config = current();
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    set_active(config.enabled);
    if !config.enabled {
        return;
    }
    thread::spawn(move || run_loop(config, generation));
}

fn run_loop(config: JournalConfig, generation: u64) {
    trace::log(format!(
        "journal:start interval_seconds={} output_dir={}",
        config.interval_seconds,
        config.output_dir.display()
    ));
    while GENERATION.load(Ordering::SeqCst) == generation && current().enabled {
        if let Err(err) = write_journal_entry(&config.output_dir) {
            trace::log(format!("journal:capture_err {err}"));
        }
        let mut slept = 0;
        while slept < config.interval_seconds {
            if GENERATION.load(Ordering::SeqCst) != generation || !current().enabled {
                trace::log("journal:stop");
                return;
            }
            thread::sleep(Duration::from_secs(1));
            slept += 1;
        }
    }
    trace::log("journal:stop");
}

fn set_active(active: bool) {
    let previous = ACTIVE.swap(active, Ordering::SeqCst);
    if previous == active {
        return;
    }
    if let Ok(hooks) = state_hooks().lock() {
        for hook in hooks.iter().copied() {
            hook(active);
        }
    }
}

fn write_journal_entry(output_dir: &Path) -> Result<(), AppError> {
    let create_result = desktop_core::paths::AppPaths::resolve()
        .ok()
        .filter(|paths| output_dir.starts_with(paths.root()))
        .map(|_| desktop_core::paths::ensure_private_dir(output_dir))
        .unwrap_or_else(|| fs::create_dir_all(output_dir));
    create_result.map_err(|err| {
        AppError::internal(format!(
            "create journal output directory {} failed: {err}",
            output_dir.display()
        ))
    })?;
    let result = daemon::execute_resident_command(Command::ScreenTokenize {
        all: false,
        overlay_out_path: None,
        window_query: None,
        screenshot_path: None,
        journal: true,
        list_all_windows: false,
        active_window: true,
        active_window_id: None,
        region: None,
    });
    let value = result?;
    let request_id = format!("journal-{}", Uuid::new_v4());
    let response = ResponseEnvelope::success(request_id, value);
    let value = serde_json::to_value(&response)
        .map_err(|err| AppError::internal(format!("encode journal response failed: {err}")))?;
    let markdown = render_tokenize_markdown(&value);
    let path = output_dir.join(format!("{}.md", timestamp_for_filename()));
    fs::write(&path, markdown).map_err(|err| {
        AppError::internal(format!("write journal {} failed: {err}", path.display()))
    })?;
    trace::log(format!("journal:capture_ok path={}", path.display()));
    Ok(())
}

fn timestamp_for_filename() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

fn render_tokenize_markdown(value: &Value) -> String {
    let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if !ok {
        let message = value
            .get("error")
            .and_then(|v| v.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return format!("# Screen Tokenize\n\n## Error\n{message}\n");
    }
    let request_id = value
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let result = value.get("result");
    let windows = result
        .and_then(|v| v.get("windows"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut lines = vec!["# Screen Tokenize".to_string(), String::new()];
    push_kv_line(&mut lines, "request_id", request_id);
    for (idx, window) in windows.iter().enumerate() {
        push_section(
            &mut lines,
            if windows.len() == 1 {
                "Window".to_string()
            } else {
                format!("Window {}", idx + 1)
            },
        );
        if let Some(app) = string_field(window, "app") {
            push_kv_line(&mut lines, "app", app);
        }
        if let Some(title) = string_field(window, "title") {
            push_kv_line(&mut lines, "window_title", title);
        }
        let elements = window
            .get("elements")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if elements.is_empty() {
            lines.push("No elements".to_string());
            continue;
        }
        for element in elements {
            if element.get("visible").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            let label = string_field(&element, "label")
                .or_else(|| string_field(&element, "text"))
                .or_else(|| string_field(&element, "role"))
                .unwrap_or_else(|| "element".to_string());
            lines.push(label);
        }
    }
    if windows.is_empty() {
        push_section(&mut lines, "Window".to_string());
        lines.push("No windows".to_string());
    }
    lines.join("\n")
}

fn push_section(lines: &mut Vec<String>, title: String) {
    lines.push(String::new());
    lines.push(format!("## {title}"));
}

fn push_kv_line(lines: &mut Vec<String>, key: &str, value: impl AsRef<str>) {
    let value = value.as_ref();
    if !value.trim().is_empty() {
        lines.push(format!("{key}: {value}"));
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}
