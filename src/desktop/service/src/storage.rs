use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use desktop_core::paths::{AppPaths, ensure_private_dir};
use serde::{Deserialize, Serialize};

use crate::{app_policy::AppPolicyConfig, journal::JournalConfig};

static CONFIG_IO: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct DesktopConfig {
    app_policy: AppPolicyConfig,
    journal: JournalConfig,
}

enum LegacyValue<T> {
    Missing,
    Valid(T),
    Blocked,
}

pub fn initialize() {
    match AppPaths::resolve() {
        Ok(paths) => {
            for message in migrate_legacy(&paths) {
                eprintln!("storage migration: {message}");
            }
        }
        Err(error) => eprintln!("storage initialization: {error}"),
    }
}

pub fn config_path() -> Result<PathBuf, String> {
    AppPaths::resolve()
        .map(|paths| paths.config_file())
        .map_err(|error| error.to_string())
}

pub fn load_app_policy() -> Result<Option<AppPolicyConfig>, String> {
    load_config().map(|config| config.map(|config| config.app_policy))
}

pub fn load_journal() -> Result<Option<JournalConfig>, String> {
    load_config().map(|config| config.map(|config| config.journal))
}

pub fn save_app_policy(policy: &AppPolicyConfig) -> Result<(), String> {
    update_config(|config| config.app_policy = policy.clone())
}

pub fn save_journal(journal: &JournalConfig) -> Result<(), String> {
    update_config(|config| config.journal = journal.clone())
}

fn load_config() -> Result<Option<DesktopConfig>, String> {
    let path = config_path()?;
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {} failed: {error}", path.display())),
    };
    toml::from_str(&raw)
        .map(Some)
        .map_err(|error| format!("invalid TOML in {}: {error}", path.display()))
}

fn update_config(update: impl FnOnce(&mut DesktopConfig)) -> Result<(), String> {
    let _guard = CONFIG_IO
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "config write lock poisoned".to_string())?;
    let mut config = load_config()?.unwrap_or_default();
    update(&mut config);
    let encoded = toml::to_string_pretty(&config)
        .map_err(|error| format!("serialize config failed: {error}"))?;
    write_private_atomic(&config_path()?, encoded.as_bytes())
}

fn migrate_legacy(paths: &AppPaths) -> Vec<String> {
    let mut messages = Vec::new();
    migrate_config(paths, &mut messages);
    migrate_agent_sessions(paths, &mut messages);
    messages
}

fn migrate_config(paths: &AppPaths, messages: &mut Vec<String>) {
    let target = paths.config_file();
    migrate_config_from_sources(
        &target,
        &legacy_config_candidates("config.json"),
        &legacy_config_candidates("journal.json"),
        messages,
    );
}

fn migrate_config_from_sources(
    target: &Path,
    policy_sources: &[PathBuf],
    journal_sources: &[PathBuf],
    messages: &mut Vec<String>,
) {
    if target.exists() {
        return;
    }
    let policy = read_unambiguous_json::<AppPolicyConfig>(policy_sources, "app policy", messages);
    let journal = read_unambiguous_json::<JournalConfig>(journal_sources, "journal", messages);
    if matches!(policy, LegacyValue::Missing) && matches!(journal, LegacyValue::Missing) {
        return;
    }
    if matches!(policy, LegacyValue::Blocked) || matches!(journal, LegacyValue::Blocked) {
        messages.push(
            "configuration migration was skipped because a legacy source is invalid or ambiguous"
                .to_string(),
        );
        return;
    }

    let config = DesktopConfig {
        app_policy: match policy {
            LegacyValue::Valid(value) => value,
            LegacyValue::Missing => AppPolicyConfig::default(),
            LegacyValue::Blocked => unreachable!(),
        },
        journal: match journal {
            LegacyValue::Valid(value) => value,
            LegacyValue::Missing => JournalConfig::default(),
            LegacyValue::Blocked => unreachable!(),
        },
    };
    match toml::to_string_pretty(&config) {
        Ok(encoded) => match write_private_new(&target, encoded.as_bytes()) {
            Ok(true) => messages.push(format!(
                "migrated legacy configuration to {}; source files were preserved",
                target.display()
            )),
            Ok(false) => {}
            Err(error) => messages.push(error),
        },
        Err(error) => messages.push(format!("failed to serialize migrated config: {error}")),
    }
}

fn migrate_agent_sessions(paths: &AppPaths, messages: &mut Vec<String>) {
    let target = paths.agent_sessions_file();
    let candidates = legacy_agent_session_candidates(paths);
    if !target.exists() && candidates.iter().any(|candidate| candidate.exists()) {
        if let Err(error) = paths.ensure_workspaces_dir() {
            messages.push(format!(
                "failed to create workspace directory {}: {error}",
                paths.workspaces_dir().display()
            ));
            return;
        }
    }
    migrate_agent_sessions_from_candidates(&target, candidates, messages);
}

fn migrate_agent_sessions_from_candidates(
    target: &Path,
    candidates: Vec<PathBuf>,
    messages: &mut Vec<String>,
) {
    if target.exists() {
        return;
    }
    let mut existing = Vec::new();
    let mut read_failed = false;
    for path in candidates {
        match fs::read(&path) {
            Ok(bytes) => existing.push((path, bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                read_failed = true;
                messages.push(format!("failed reading legacy {}: {error}", path.display()));
            }
        }
    }
    if read_failed {
        messages.push("agent session migration was skipped after a read error".to_string());
        return;
    }
    let Some((source, bytes)) = unambiguous_bytes(existing, "agent session", messages) else {
        return;
    };
    if let Err(error) = serde_json::from_slice::<serde_json::Value>(&bytes) {
        messages.push(format!(
            "legacy agent session file {} is invalid JSON ({error}); it was preserved",
            source.display()
        ));
        return;
    }
    match write_private_new(&target, &bytes) {
        Ok(true) => messages.push(format!(
            "migrated agent sessions from {} to {}; source file was preserved",
            source.display(),
            target.display()
        )),
        Ok(false) => {}
        Err(error) => messages.push(error),
    }
}

fn legacy_config_candidates(file_name: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        paths.push(PathBuf::from(base).join("desktopctl").join(file_name));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        paths.push(
            PathBuf::from(home)
                .join(".config")
                .join("desktopctl")
                .join(file_name),
        );
    }
    paths.sort();
    paths.dedup();
    paths
}

fn legacy_agent_session_candidates(paths: &AppPaths) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        let home = PathBuf::from(home);
        candidates.push(
            home.join("Library")
                .join("Application Support")
                .join("DesktopCtl")
                .join("agent-sessions.json"),
        );
        candidates.push(
            home.join(".local")
                .join("share")
                .join("desktopctl")
                .join("agent-sessions.json"),
        );
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        candidates.push(
            PathBuf::from(xdg)
                .join("desktopctl")
                .join("agent-sessions.json"),
        );
    }
    candidates.retain(|candidate| candidate != &paths.agent_sessions_file());
    candidates.sort();
    candidates.dedup();
    candidates
}

fn read_unambiguous_json<T>(
    candidates: &[PathBuf],
    label: &str,
    messages: &mut Vec<String>,
) -> LegacyValue<T>
where
    T: serde::de::DeserializeOwned,
{
    let mut existing = Vec::new();
    let mut read_failed = false;
    for path in candidates {
        match fs::read(path) {
            Ok(bytes) => existing.push((path.clone(), bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                read_failed = true;
                messages.push(format!("failed reading legacy {}: {error}", path.display()));
            }
        }
    }
    if read_failed {
        return LegacyValue::Blocked;
    }
    if existing.is_empty() {
        return LegacyValue::Missing;
    }
    let Some((path, bytes)) = unambiguous_bytes(existing, label, messages) else {
        return LegacyValue::Blocked;
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => LegacyValue::Valid(value),
        Err(error) => {
            messages.push(format!(
                "legacy {label} file {} is invalid JSON ({error}); it was preserved",
                path.display()
            ));
            LegacyValue::Blocked
        }
    }
}

fn unambiguous_bytes(
    existing: Vec<(PathBuf, Vec<u8>)>,
    label: &str,
    messages: &mut Vec<String>,
) -> Option<(PathBuf, Vec<u8>)> {
    let mut iter = existing.into_iter();
    let first = iter.next()?;
    let conflicts: Vec<PathBuf> = iter
        .filter_map(|(path, bytes)| (bytes != first.1).then_some(path))
        .collect();
    if !conflicts.is_empty() {
        let mut paths = vec![first.0.clone()];
        paths.extend(conflicts);
        messages.push(format!(
            "ambiguous legacy {label} files were preserved and not migrated: {}",
            paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        return None;
    }
    Some(first)
}

fn write_private_new(path: &Path, bytes: &[u8]) -> Result<bool, String> {
    let Some(parent) = path.parent() else {
        return Err(format!("{} has no parent directory", path.display()));
    };
    ensure_private_dir(parent)
        .map_err(|error| format!("create {} failed: {error}", parent.display()))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_private_file_mode(&mut options);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(format!("create {} failed: {error}", path.display())),
    };
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(path);
        return Err(format!("write {} failed: {error}", path.display()));
    }
    Ok(true)
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err(format!("{} has no parent directory", path.display()));
    };
    ensure_private_dir(parent)
        .map_err(|error| format!("create {} failed: {error}", parent.display()))?;
    let temp = parent.join(format!(".config.toml.tmp-{}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    set_private_file_mode(&mut options);
    let result = (|| {
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(format!("write {} failed: {error}", path.display()));
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_file_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_file_mode(_options: &mut OpenOptions) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "desktopctl-storage-{label}-{}-{}",
            std::process::id(),
            timestamp
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn conflicting_sources_are_not_selected() {
        let root = temp_dir("conflict");
        let first = root.join("one.json");
        let second = root.join("two.json");
        fs::write(&first, b"one").unwrap();
        fs::write(&second, b"two").unwrap();
        let mut messages = Vec::new();
        assert!(
            unambiguous_bytes(
                vec![(first, b"one".to_vec()), (second, b"two".to_vec())],
                "test",
                &mut messages,
            )
            .is_none()
        );
        assert!(messages[0].contains("ambiguous"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn private_new_write_is_idempotent() {
        let root = temp_dir("new");
        let path = root.join("nested").join("value");
        assert!(write_private_new(&path, b"first").unwrap());
        assert!(!write_private_new(&path, b"second").unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"first");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn config_migration_converts_both_json_files_and_preserves_sources() {
        let root = temp_dir("config-migration");
        let policy_path = root.join("legacy-config.json");
        let journal_path = root.join("legacy-journal.json");
        let target = root.join("new").join("config.toml");
        let policy = AppPolicyConfig {
            agent_access_disabled: true,
            ..AppPolicyConfig::default()
        };
        let journal = JournalConfig {
            enabled: true,
            ..JournalConfig::default()
        };
        fs::write(&policy_path, serde_json::to_vec(&policy).unwrap()).unwrap();
        fs::write(&journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();

        let mut messages = Vec::new();
        migrate_config_from_sources(
            &target,
            std::slice::from_ref(&policy_path),
            std::slice::from_ref(&journal_path),
            &mut messages,
        );

        let migrated: DesktopConfig =
            toml::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
        assert!(migrated.app_policy.agent_access_disabled);
        assert!(migrated.journal.enabled);
        assert!(policy_path.exists());
        assert!(journal_path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn config_migration_never_overwrites_existing_target() {
        let root = temp_dir("config-existing");
        let source = root.join("legacy.json");
        let target = root.join("config.toml");
        fs::write(
            &source,
            serde_json::to_vec(&AppPolicyConfig::default()).unwrap(),
        )
        .unwrap();
        fs::write(&target, "existing = true\n").unwrap();

        migrate_config_from_sources(&target, &[source], &[], &mut Vec::new());

        assert_eq!(fs::read_to_string(&target).unwrap(), "existing = true\n");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_config_source_blocks_partial_migration() {
        let root = temp_dir("config-invalid");
        let policy = root.join("policy.json");
        let journal = root.join("journal.json");
        let target = root.join("config.toml");
        fs::write(&policy, b"not json").unwrap();
        fs::write(
            &journal,
            serde_json::to_vec(&JournalConfig::default()).unwrap(),
        )
        .unwrap();
        let mut messages = Vec::new();

        migrate_config_from_sources(&target, &[policy], &[journal], &mut messages);

        assert!(!target.exists());
        assert!(messages.iter().any(|message| message.contains("skipped")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn session_migration_is_copy_only_and_idempotent() {
        let root = temp_dir("session-migration");
        let source = root.join("agent-sessions.json");
        let target = root.join("workspaces").join("agent-sessions.json");
        let bytes = br#"{"version":1,"sessions":[]}"#;
        fs::write(&source, bytes).unwrap();

        migrate_agent_sessions_from_candidates(&target, vec![source.clone()], &mut Vec::new());
        assert_eq!(fs::read(&target).unwrap(), bytes);
        assert_eq!(fs::read(&source).unwrap(), bytes);

        fs::write(&target, b"newer").unwrap();
        migrate_agent_sessions_from_candidates(&target, vec![source], &mut Vec::new());
        assert_eq!(fs::read(&target).unwrap(), b"newer");
        let _ = fs::remove_dir_all(root);
    }
}
