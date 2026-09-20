use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use thiserror::Error;

use crate::model::{Element, MenuItem, Observation, Window};

#[derive(Debug, Error)]
pub enum DesktopCtlError {
    #[error("desktopctl executable not found")]
    NotFound,
    #[error("desktopctl timed out after {0} ms")]
    Timeout(u128),
    #[error("desktopctl failed: {0}")]
    Failed(String),
    #[error("invalid desktopctl JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct DesktopCtl {
    binary: PathBuf,
    timeout: Duration,
    active_window: Option<String>,
}

/// A screen tokenization worker. Dropping the handle intentionally detaches
/// the worker; the worker still reaps its child process in `invoke`.
pub struct ScreenObservation {
    handle: thread::JoinHandle<Result<Observation, DesktopCtlError>>,
    pub started: Instant,
}

impl ScreenObservation {
    pub fn join(self) -> Result<Result<Observation, DesktopCtlError>, DesktopCtlError> {
        self.handle
            .join()
            .map_err(|_| DesktopCtlError::Failed("screen observation thread panicked".into()))
    }
}

impl DesktopCtl {
    pub fn new(timeout: Duration, active_window: Option<String>) -> Result<Self, DesktopCtlError> {
        let binary = std::env::var_os("DESKTOPCTL_PATH")
            .map(PathBuf::from)
            .or_else(|| {
                [
                    "./dist/desktopctl",
                    "./target/release/desktopctl",
                    "desktopctl",
                ]
                .iter()
                .map(PathBuf::from)
                .find(|path| path == &PathBuf::from("desktopctl") || path.is_file())
            })
            .ok_or(DesktopCtlError::NotFound)?;
        Ok(Self {
            binary,
            timeout,
            active_window,
        })
    }

    pub fn observe(&self) -> Result<Observation, DesktopCtlError> {
        let mut observation = self.observe_screen()?;
        observation.menus = self.observe_menu_only()?;
        Ok(observation)
    }

    pub fn observe_menu_only(&self) -> Result<Vec<MenuItem>, DesktopCtlError> {
        self.observe_menu()
    }

    /// Start tokenization after menu discovery. This ordering matters because
    /// the DesktopCtl daemon serializes commands globally.
    pub fn start_screen_tokenize(&self) -> ScreenObservation {
        let client = self.clone();
        let started = Instant::now();
        let handle = thread::spawn(move || client.observe_screen());
        ScreenObservation { handle, started }
    }

    fn observe_screen(&self) -> Result<Observation, DesktopCtlError> {
        let mut args = vec!["--json".to_string(), "screen".into(), "tokenize".into()];
        if let Some(window) = &self.active_window {
            args.extend(["--active-window".into(), window.clone()]);
        }
        let raw = self.invoke(&args)?;
        let mut observation = normalize_observation(raw);
        if let Some(window) = &self.active_window {
            observation.active_window_id = Some(window.clone());
        }
        Ok(observation)
    }

    fn observe_menu(&self) -> Result<Vec<MenuItem>, DesktopCtlError> {
        let Some(window) = &self.active_window else {
            return Ok(Vec::new());
        };
        let menu_args = vec![
            "--json".into(),
            "menu".into(),
            "list".into(),
            "--active-window".into(),
            window.clone(),
        ];
        let menu_raw = self.invoke(&menu_args)?;
        Ok(extract_menus(&menu_raw))
    }

    pub fn execute(&self, action: &crate::model::Candidate) -> Result<Value, DesktopCtlError> {
        use crate::model::ActionKind;
        let mut args = vec!["--json".to_string()];
        match action.kind {
            ActionKind::Click => {
                args.extend([
                    "pointer".into(),
                    "click".into(),
                    "--id".into(),
                    action
                        .target
                        .clone()
                        .ok_or_else(|| DesktopCtlError::Failed("click has no target".into()))?,
                ]);
            }
            ActionKind::Menu => {
                let _ = self.active_window.as_ref().ok_or_else(|| {
                    DesktopCtlError::Failed("menu action has no active window".into())
                })?;
                args.extend([
                    "menu".into(),
                    "click".into(),
                    "--id".into(),
                    action
                        .target
                        .clone()
                        .ok_or_else(|| DesktopCtlError::Failed("menu has no target".into()))?,
                ]);
            }
            ActionKind::ScrollUp | ActionKind::ScrollDown => {
                args.extend(["pointer".into(), "scroll".into()]);
                if let Some(id) = action.target.clone() {
                    args.extend(["--id".into(), id]);
                }
                args.extend([
                    "0".into(),
                    if action.kind == ActionKind::ScrollUp {
                        "-600"
                    } else {
                        "600"
                    }
                    .into(),
                ]);
            }
            ActionKind::Type => args.extend([
                "keyboard".into(),
                "type".into(),
                action
                    .literal
                    .clone()
                    .ok_or_else(|| DesktopCtlError::Failed("type has no literal".into()))?,
            ]),
            ActionKind::PressEnter | ActionKind::PressTab | ActionKind::PressEscape => {
                let key = match action.kind {
                    ActionKind::PressEnter => "enter",
                    ActionKind::PressTab => "tab",
                    _ => "escape",
                };
                args.extend(["keyboard".into(), "press".into(), key.into()]);
            }
            ActionKind::Wait => {
                thread::sleep(Duration::from_millis(250));
                return Ok(Value::Null);
            }
            ActionKind::Done | ActionKind::Blocked => return Ok(Value::Null),
        }
        if let Some(window) = &self.active_window {
            args.extend(["--active-window".into(), window.clone()]);
        }
        self.invoke(&args)
    }

    fn invoke(&self, args: &[String]) -> Result<Value, DesktopCtlError> {
        let mut child = Command::new(&self.binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| DesktopCtlError::Failed(error.to_string()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| DesktopCtlError::Failed("desktopctl stdout was not captured".into()))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| DesktopCtlError::Failed("desktopctl stderr was not captured".into()))?;
        let stdout_reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.read_to_end(&mut bytes);
            (result, bytes)
        });
        let stderr_reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stderr.read_to_end(&mut bytes);
            (result, bytes)
        });
        let started = Instant::now();
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| DesktopCtlError::Failed(error.to_string()))?
            {
                let stdout = join_reader(stdout_reader, "stdout")?;
                let stderr = join_reader(stderr_reader, "stderr")?;
                let text = String::from_utf8_lossy(&stdout).trim().to_string();
                if !status.success() {
                    let error = String::from_utf8_lossy(&stderr).trim().to_string();
                    return Err(DesktopCtlError::Failed(if !text.is_empty() {
                        text
                    } else if !error.is_empty() {
                        error
                    } else {
                        "command failed".into()
                    }));
                }
                return Ok(serde_json::from_str(&text)?);
            }
            if started.elapsed() >= self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(DesktopCtlError::Timeout(started.elapsed().as_millis()));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn join_reader(
    reader: thread::JoinHandle<(std::io::Result<usize>, Vec<u8>)>,
    stream: &str,
) -> Result<Vec<u8>, DesktopCtlError> {
    let (result, bytes) = reader
        .join()
        .map_err(|_| DesktopCtlError::Failed(format!("desktopctl {stream} reader panicked")))?;
    result.map_err(|error| DesktopCtlError::Failed(error.to_string()))?;
    Ok(bytes)
}

fn result_value(raw: &Value) -> &Value {
    raw.get("result").unwrap_or(raw)
}

pub fn focused_element_id(raw: &Value) -> Option<String> {
    let result = result_value(raw);
    result
        .get("focused_element_id")
        .and_then(Value::as_str)
        .or_else(|| {
            result
                .get("observe")
                .and_then(|value| value.get("focused_element_id"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

fn normalize_observation(raw: Value) -> Observation {
    let result = result_value(&raw);
    let windows = result.get("windows").and_then(Value::as_array);
    let selected = windows.and_then(|items| items.first());
    let window = Window {
        id: selected
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        app: selected
            .and_then(|v| v.get("app"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        title: selected
            .and_then(|v| v.get("title"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    };
    let elements = selected
        .and_then(|v| v.get("elements"))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_element).collect())
        .unwrap_or_default();
    Observation {
        active_window_id: result
            .get("active_window_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        focused_element_id: focused_element_id(&raw),
        window,
        elements,
        menus: Vec::new(),
    }
}

fn parse_element(value: &Value) -> Option<Element> {
    let id = value.get("id")?.as_str()?.to_owned();
    let role = value
        .get("type")
        .or_else(|| value.get("role"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let bbox = value.get("bbox").and_then(Value::as_array).and_then(|v| {
        (v.len() == 4).then(|| {
            [
                v[0].as_f64().unwrap_or(0.0),
                v[1].as_f64().unwrap_or(0.0),
                v[2].as_f64().unwrap_or(0.0),
                v[3].as_f64().unwrap_or(0.0),
            ]
        })
    });
    Some(Element {
        id,
        role,
        text: value.get("text").and_then(Value::as_str).map(str::to_owned),
        bbox,
        scrollable: value
            .get("scrollable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        checked: value
            .get("checked")
            .and_then(Value::as_str)
            .map(str::to_owned),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn extract_menus(raw: &Value) -> Vec<MenuItem> {
    let mut out = Vec::new();
    collect_menus(result_value(raw), &mut out, None);
    out
}

fn collect_menus(value: &Value, out: &mut Vec<MenuItem>, parent: Option<&str>) {
    match value {
        Value::Array(items) => items
            .iter()
            .for_each(|item| collect_menus(item, out, parent)),
        Value::Object(object) => {
            let title = object
                .get("title")
                .or_else(|| object.get("name"))
                .and_then(Value::as_str);
            let id = object.get("id").and_then(Value::as_str);
            if let (Some(title), Some(id)) = (title, id) {
                let path = parent
                    .map(|p| format!("{p} > {title}"))
                    .unwrap_or_else(|| title.to_owned());
                out.push(MenuItem {
                    id: id.to_owned(),
                    path: path.clone(),
                    title: title.to_owned(),
                    enabled: object
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                    action_supported: object
                        .get("action_supported")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                });
                if let Some(children) = object.get("items").or_else(|| object.get("children")) {
                    collect_menus(children, out, Some(&path));
                }
            } else {
                object
                    .values()
                    .for_each(|item| collect_menus(item, out, parent));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_tokenize_payload() {
        let observation = normalize_observation(
            serde_json::json!({"ok":true,"result":{"windows":[{"id":"w1","title":"Inbox","elements":[{"id":"e1","type":"AXButton","text":"Reply","bbox":[1,2,3,4],"source":"ax"}]}]}}),
        );
        assert_eq!(observation.window.id.as_deref(), Some("w1"));
        assert_eq!(observation.elements[0].id, "e1");
    }

    #[test]
    fn extracts_focused_element_from_action_observation() {
        let raw = serde_json::json!({
            "ok": true,
            "result": {"observe": {"focused_element_id": "search-field"}}
        });
        assert_eq!(focused_element_id(&raw).as_deref(), Some("search-field"));
    }
}
