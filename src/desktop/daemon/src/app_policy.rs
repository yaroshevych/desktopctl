use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use desktop_core::protocol::Command;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PolicyMode {
    #[default]
    AllowAll,
    AllowOnlySelected,
    AllowAllExcept,
}

fn default_allow_full_screen_capture() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppPolicyConfig {
    #[serde(default)]
    pub policy_mode: PolicyMode,
    #[serde(default)]
    pub apps: Vec<String>,
    #[serde(default = "default_allow_full_screen_capture")]
    pub allow_full_screen_capture: bool,
    #[serde(default)]
    pub agent_access_disabled: bool,
}

impl Default for AppPolicyConfig {
    fn default() -> Self {
        Self {
            policy_mode: PolicyMode::AllowAll,
            apps: Vec::new(),
            allow_full_screen_capture: default_allow_full_screen_capture(),
            agent_access_disabled: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadOutcome {
    pub config: AppPolicyConfig,
    pub warning: Option<String>,
}

static CURRENT_POLICY: OnceLock<Mutex<AppPolicyConfig>> = OnceLock::new();

fn current_policy() -> &'static Mutex<AppPolicyConfig> {
    CURRENT_POLICY.get_or_init(|| Mutex::new(AppPolicyConfig::default()))
}

pub fn reload_current_from_disk() -> LoadOutcome {
    let outcome = load_with_diagnostics();
    if let Ok(mut guard) = current_policy().lock() {
        *guard = outcome.config.clone();
    } else {
        eprintln!("app policy: failed to update in-memory policy (lock poisoned)");
    }
    outcome
}

pub fn current() -> AppPolicyConfig {
    current_policy()
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_else(|_| {
            eprintln!(
                "app policy: failed to read in-memory policy (lock poisoned), using defaults"
            );
            AppPolicyConfig::default()
        })
}

pub fn set_current(cfg: &AppPolicyConfig) {
    if let Ok(mut guard) = current_policy().lock() {
        let mut normalized = cfg.clone();
        normalized.apps = normalize_apps(&normalized.apps);
        *guard = normalized;
    } else {
        eprintln!("app policy: failed to update in-memory policy (lock poisoned)");
    }
}

pub fn set_agent_access_disabled(disabled: bool) -> Result<(), String> {
    let mut cfg = current();
    cfg.agent_access_disabled = disabled;
    save(&cfg)?;
    set_current(&cfg);
    Ok(())
}

pub fn config_path() -> Option<PathBuf> {
    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME") {
        let trimmed = PathBuf::from(base);
        if !trimmed.as_os_str().is_empty() {
            return Some(trimmed.join("desktopctl").join("config.json"));
        }
    }

    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/desktopctl/config.json"))
}

pub fn load_with_diagnostics() -> LoadOutcome {
    let Some(path) = config_path() else {
        let warning = "unable to resolve app policy config path; using defaults".to_string();
        eprintln!("app policy: {warning}");
        return LoadOutcome {
            config: AppPolicyConfig::default(),
            warning: Some(warning),
        };
    };

    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                let warning = format!("failed reading {}: {err}; using defaults", path.display());
                eprintln!("app policy: {warning}");
                return LoadOutcome {
                    config: AppPolicyConfig::default(),
                    warning: Some(warning),
                };
            }
            return LoadOutcome {
                config: AppPolicyConfig::default(),
                warning: None,
            };
        }
    };

    let mut cfg: AppPolicyConfig = match serde_json::from_str(&raw) {
        Ok(cfg) => cfg,
        Err(err) => {
            let warning = format!("invalid JSON in {}: {err}; using defaults", path.display());
            eprintln!("app policy: {warning}");
            return LoadOutcome {
                config: AppPolicyConfig::default(),
                warning: Some(warning),
            };
        }
    };

    cfg.apps = normalize_apps(&cfg.apps);
    LoadOutcome {
        config: cfg,
        warning: None,
    }
}

pub fn save(cfg: &AppPolicyConfig) -> Result<(), String> {
    let Some(path) = config_path() else {
        return Err("unable to resolve config path".to_string());
    };
    ensure_parent_dir(&path)?;

    let mut normalized = cfg.clone();
    normalized.apps = normalize_apps(&normalized.apps);

    let encoded = serde_json::to_vec_pretty(&normalized)
        .map_err(|err| format!("serialize config failed: {err}"))?;
    fs::write(&path, encoded)
        .map_err(|err| format!("write config {} failed: {err}", path.display()))
}

fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err("config path has no parent directory".to_string());
    };
    fs::create_dir_all(parent)
        .map_err(|err| format!("create config directory {} failed: {err}", parent.display()))
}

pub fn normalize_apps_csv(csv: &str) -> Vec<String> {
    let apps: Vec<String> = csv
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
        .collect();
    normalize_apps(&apps)
}

pub fn apps_to_csv(apps: &[String]) -> String {
    apps.join(", ")
}

pub fn is_app_allowed(cfg: &AppPolicyConfig, frontmost_app: &str) -> bool {
    match cfg.policy_mode {
        PolicyMode::AllowAll => true,
        PolicyMode::AllowOnlySelected => cfg
            .apps
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(frontmost_app)),
        PolicyMode::AllowAllExcept => !cfg
            .apps
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(frontmost_app)),
    }
}

pub fn command_requires_policy(command: &Command) -> bool {
    matches!(
        command,
        Command::ScreenCapture { .. }
            | Command::OpenApp { .. }
            | Command::ScreenTokenize { .. }
            | Command::ScreenFindText { .. }
            | Command::WaitText { .. }
            | Command::PointerMove { .. }
            | Command::PointerDown { .. }
            | Command::PointerUp { .. }
            | Command::PointerClick { .. }
            | Command::PointerClickText { .. }
            | Command::PointerClickId { .. }
            | Command::PointerScroll { .. }
            | Command::PointerDrag { .. }
            | Command::UiType { .. }
            | Command::KeyHotkey { .. }
            | Command::KeyEnter { .. }
            | Command::KeyEscape { .. }
    )
}

pub fn command_target_app_name(command: &Command) -> Option<&str> {
    match command {
        Command::OpenApp { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

pub fn command_is_full_screen_capture(command: &Command) -> bool {
    matches!(
        command,
        Command::ScreenCapture {
            active_window: false,
            region: None,
            ..
        }
    )
}

fn normalize_apps(apps: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for app in apps {
        let trimmed = app.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(trimmed.to_string());
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_apps_csv_trims_dedupes_and_drops_empty() {
        let apps = normalize_apps_csv(" Safari,Slack, safari , , Terminal  ");
        assert_eq!(apps, vec!["Safari", "Slack", "Terminal"]);
    }

    #[test]
    fn allow_only_selected_requires_match() {
        let cfg = AppPolicyConfig {
            policy_mode: PolicyMode::AllowOnlySelected,
            apps: vec!["Safari".to_string(), "Slack".to_string()],
            allow_full_screen_capture: false,
            agent_access_disabled: false,
        };
        assert!(is_app_allowed(&cfg, "safari"));
        assert!(!is_app_allowed(&cfg, "Terminal"));
    }

    #[test]
    fn allow_all_except_blocks_matching_apps() {
        let cfg = AppPolicyConfig {
            policy_mode: PolicyMode::AllowAllExcept,
            apps: vec!["Slack".to_string()],
            allow_full_screen_capture: false,
            agent_access_disabled: false,
        };
        assert!(!is_app_allowed(&cfg, "slack"));
        assert!(is_app_allowed(&cfg, "Safari"));
    }

    #[test]
    fn default_config_enables_full_screen_capture() {
        let cfg = AppPolicyConfig::default();
        assert!(cfg.allow_full_screen_capture);
    }

    #[test]
    fn deserialize_missing_allow_full_screen_capture_defaults_to_true() {
        let cfg: AppPolicyConfig =
            serde_json::from_str(r#"{ "policy_mode": "allow_all", "apps": [] }"#)
                .expect("config should deserialize");
        assert!(cfg.allow_full_screen_capture);
        assert!(!cfg.agent_access_disabled);
    }

    #[test]
    fn open_app_requires_policy_and_exposes_target_name() {
        let command = Command::OpenApp {
            name: "Notes".to_string(),
            args: Vec::new(),
            wait: false,
            timeout_ms: None,
        };
        assert!(command_requires_policy(&command));
        assert_eq!(command_target_app_name(&command), Some("Notes"));
    }
}
