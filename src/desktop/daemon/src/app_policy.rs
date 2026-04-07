use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AppPolicyConfig {
    #[serde(default)]
    pub policy_mode: PolicyMode,
    #[serde(default)]
    pub apps: Vec<String>,
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

pub fn load() -> AppPolicyConfig {
    let Some(path) = config_path() else {
        return AppPolicyConfig::default();
    };

    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => return AppPolicyConfig::default(),
    };

    let mut cfg: AppPolicyConfig = match serde_json::from_str(&raw) {
        Ok(cfg) => cfg,
        Err(_) => return AppPolicyConfig::default(),
    };

    cfg.apps = normalize_apps(&cfg.apps);
    cfg
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
        };
        assert!(is_app_allowed(&cfg, "safari"));
        assert!(!is_app_allowed(&cfg, "Terminal"));
    }

    #[test]
    fn allow_all_except_blocks_matching_apps() {
        let cfg = AppPolicyConfig {
            policy_mode: PolicyMode::AllowAllExcept,
            apps: vec!["Slack".to_string()],
        };
        assert!(!is_app_allowed(&cfg, "slack"));
        assert!(is_app_allowed(&cfg, "Safari"));
    }
}
