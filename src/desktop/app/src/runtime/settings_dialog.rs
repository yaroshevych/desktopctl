use std::{
    collections::HashSet,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    thread,
};

use serde::{Deserialize, Serialize};

use crate::service_client::ServiceClient;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PolicyMode {
    AllowAll,
    AllowOnlySelected,
    AllowAllExcept,
}

#[derive(Deserialize, Serialize)]
struct JournalInput {
    enabled: bool,
    interval_seconds: u64,
    output_dir: String,
}

#[derive(Deserialize, Serialize)]
struct AppPolicyInput {
    policy_mode: PolicyMode,
    apps: Vec<String>,
    allow_full_screen_capture: bool,
    clipboard_allowed: bool,
    warning: Option<String>,
}

#[derive(Deserialize)]
struct StoredAppPolicy {
    policy_mode: PolicyMode,
    apps: Vec<String>,
    allow_full_screen_capture: bool,
    clipboard_allowed: bool,
}

#[derive(Deserialize)]
struct StoredSettings {
    journal: JournalInput,
    app_policy: StoredAppPolicy,
    app_policy_warning: Option<String>,
}

#[derive(Serialize)]
struct SetupAccessInput {
    cli_installed: bool,
    accessibility_granted: bool,
    screen_recording_granted: bool,
    cli_source: Option<String>,
    candidate_cli_dirs: Vec<String>,
}

#[derive(Serialize)]
struct SettingsInput {
    journal: JournalInput,
    app_policy: AppPolicyInput,
    setup_access: SetupAccessInput,
    initial_tab: Option<String>,
}

#[derive(Deserialize)]
struct JournalOutput {
    saved: bool,
    enabled: bool,
    interval_seconds: u64,
    output_dir: String,
}

#[derive(Deserialize)]
struct AppPolicyOutput {
    saved: bool,
    policy_mode: PolicyMode,
    apps: Vec<String>,
    allow_full_screen_capture: bool,
    clipboard_allowed: bool,
}

#[derive(Deserialize)]
struct SettingsOutput {
    journal: JournalOutput,
    app_policy: AppPolicyOutput,
}

static ACTIVE_DIALOG_PIDS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

fn active_dialog_pids() -> &'static Mutex<HashSet<u32>> {
    ACTIVE_DIALOG_PIDS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn dialog_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("DESKTOPCTL_DIALOGS_BIN") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let path = parent.join("desktopctl-dialogs");
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}

pub fn terminate_active() {
    let pids: Vec<u32> = active_dialog_pids()
        .lock()
        .map(|pids| pids.iter().copied().collect())
        .unwrap_or_default();

    for pid in pids {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status();
    }
}

pub fn show(initial_tab: Option<&'static str>) {
    thread::spawn(move || {
        let Some(bin) = dialog_binary() else {
            eprintln!("settings dialog: desktopctl-dialogs binary not found");
            return;
        };

        let client = ServiceClient;
        let stored: StoredSettings = match client.settings().and_then(|value| {
            serde_json::from_value(value).map_err(|error| {
                desktop_core::error::AppError::internal(format!(
                    "decode settings response failed: {error}"
                ))
            })
        }) {
            Ok(settings) => settings,
            Err(error) => {
                eprintln!("settings dialog: load service settings: {error}");
                return;
            }
        };
        let permissions: desktop_core::protocol::PermissionsPayload =
            match client.send_typed(desktop_core::protocol::Command::PermissionsCheck) {
                Ok(permissions) => permissions,
                Err(error) => {
                    eprintln!("settings dialog: load permissions: {error}");
                    return;
                }
            };

        let input = SettingsInput {
            journal: JournalInput {
                enabled: stored.journal.enabled,
                interval_seconds: stored.journal.interval_seconds,
                output_dir: stored.journal.output_dir,
            },
            app_policy: AppPolicyInput {
                policy_mode: stored.app_policy.policy_mode,
                apps: stored.app_policy.apps,
                allow_full_screen_capture: stored.app_policy.allow_full_screen_capture,
                clipboard_allowed: stored.app_policy.clipboard_allowed,
                warning: stored.app_policy_warning,
            },
            setup_access: SetupAccessInput {
                cli_installed: cli_in_path(),
                accessibility_granted: permissions.accessibility.granted,
                screen_recording_granted: permissions.screen_recording.granted,
                cli_source: discover_cli_binary().map(|p| p.display().to_string()),
                candidate_cli_dirs: candidate_cli_dirs()
                    .into_iter()
                    .map(|p| p.display().to_string())
                    .collect(),
            },
            initial_tab: initial_tab.map(|s| s.to_string()),
        };

        let input_json = match serde_json::to_vec(&input) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("settings dialog: serialize input: {e}");
                return;
            }
        };

        let mut child = match Command::new(&bin)
            .arg("settings")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("settings dialog: spawn: {e}");
                return;
            }
        };
        let child_id = child.id();
        if let Ok(mut pids) = active_dialog_pids().lock() {
            pids.insert(child_id);
        }
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&input_json);
        }
        let out = match child.wait_with_output() {
            Ok(o) => o,
            Err(e) => {
                if let Ok(mut pids) = active_dialog_pids().lock() {
                    pids.remove(&child_id);
                }
                eprintln!("settings dialog: wait: {e}");
                return;
            }
        };
        if let Ok(mut pids) = active_dialog_pids().lock() {
            pids.remove(&child_id);
        }
        if out.stdout.is_empty() {
            return;
        }
        let output: SettingsOutput = match serde_json::from_slice(&out.stdout) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("settings dialog: parse output: {e}");
                return;
            }
        };

        let journal = output.journal.saved.then(|| {
            serde_json::json!({
                "enabled": output.journal.enabled,
                "interval_seconds": output.journal.interval_seconds.max(1),
                "output_dir": output.journal.output_dir,
            })
        });
        let app_policy = output.app_policy.saved.then(|| {
            serde_json::json!({
                "policy_mode": output.app_policy.policy_mode,
                "apps": output.app_policy.apps,
                "allow_full_screen_capture": output.app_policy.allow_full_screen_capture,
                "clipboard_allowed": output.app_policy.clipboard_allowed,
            })
        });
        if (journal.is_some() || app_policy.is_some())
            && let Err(error) = client.update_settings(journal, app_policy)
        {
            eprintln!("settings dialog: save service settings: {error}");
        }
    });
}

fn cli_in_path() -> bool {
    if Command::new("which")
        .arg("desktopctl")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return true;
    }
    discover_existing_cli_link().is_some()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn candidate_cli_dirs() -> Vec<PathBuf> {
    let mut candidate_dirs: Vec<PathBuf> = std::env::var("PATH")
        .ok()
        .map(|path| {
            path.split(':')
                .filter(|segment| !segment.trim().is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default();

    if home_dir().is_some() {
        candidate_dirs.push(PathBuf::from("/usr/local/bin"));
        candidate_dirs.push(PathBuf::from("/opt/homebrew/bin"));
        if let Some(home) = home_dir() {
            candidate_dirs.push(home.join(".local/bin"));
            candidate_dirs.push(home.join("bin"));
        }
    }

    let mut seen: HashSet<PathBuf> = HashSet::new();
    candidate_dirs.retain(|dir| seen.insert(dir.clone()));
    candidate_dirs
}

fn discover_existing_cli_link() -> Option<PathBuf> {
    for dir in candidate_cli_dirs() {
        let link_path = dir.join("desktopctl");
        if link_path.exists() {
            return Some(link_path);
        }
    }
    None
}

fn discover_cli_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("DESKTOPCTL_CLI_BIN") {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;

    let sibling = exe_dir.join("desktopctl");
    if sibling.exists() {
        return Some(sibling);
    }

    let bundled = exe_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("MacOS/desktopctl"));
    if let Some(candidate) = bundled {
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}
