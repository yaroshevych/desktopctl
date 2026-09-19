use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Observation {
    pub active_window_id: Option<String>,
    pub window: Window,
    pub focused_element_id: Option<String>,
    pub elements: Vec<Element>,
    #[serde(default)]
    pub menus: Vec<MenuItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Window {
    pub id: Option<String>,
    pub app: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Element {
    pub id: String,
    pub role: String,
    pub text: Option<String>,
    pub bbox: Option<[f64; 4]>,
    pub scrollable: bool,
    pub checked: Option<String>,
    pub source: Option<String>,
}

impl Element {
    pub fn label(&self) -> String {
        self.text.clone().unwrap_or_else(|| self.role.clone())
    }

    pub fn normalized_role(&self) -> String {
        self.role
            .to_ascii_lowercase()
            .replace(['_', '-'], " ")
            .trim_start_matches("ax")
            .trim()
            .to_string()
    }

    pub fn is_editable(&self) -> bool {
        let role = self.normalized_role();
        role.contains("text field")
            || role.contains("textfield")
            || role.contains("search")
            || role.contains("textarea")
            || role.contains("text area")
            || role.contains("editable")
            || role == "text field"
            || role == "search field"
    }

    pub fn is_clickable(&self) -> bool {
        let role = self.normalized_role();
        [
            "button",
            "link",
            "checkbox",
            "radio",
            "switch",
            "tab",
            "text field",
            "textfield",
            "search field",
            "textarea",
            "text area",
            "combobox",
            "menu",
            "menu item",
            "popup button",
            "slider",
            "stepper",
            "incrementor",
        ]
        .iter()
        .any(|known| role == *known || role.contains(known))
            || role.contains("search")
            || role.contains("text field")
            || self
                .source
                .as_deref()
                .map(|s| s.to_ascii_lowercase().contains("button"))
                .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MenuItem {
    pub id: String,
    pub path: String,
    pub title: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub action_supported: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub kind: ActionKind,
    pub target: Option<String>,
    pub literal: Option<String>,
    pub terminal: bool,
    pub status: Option<TerminalStatus>,
    pub description: String,
    pub criteria: String,
}

impl Candidate {
    pub fn action(kind: ActionKind, description: String, criteria: String) -> Self {
        Self {
            id: String::new(),
            kind,
            target: None,
            literal: None,
            terminal: false,
            status: None,
            description,
            criteria,
        }
    }

    pub fn terminal(status: TerminalStatus, description: impl Into<String>) -> Self {
        let description = description.into();
        Self {
            id: String::new(),
            kind: match status {
                TerminalStatus::Done => ActionKind::Done,
                TerminalStatus::Blocked => ActionKind::Blocked,
            },
            target: None,
            literal: None,
            terminal: true,
            status: Some(status),
            criteria: description.clone(),
            description,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Click,
    Menu,
    ScrollUp,
    ScrollDown,
    Type,
    PressEnter,
    PressTab,
    PressEscape,
    Wait,
    Done,
    Blocked,
}

impl ActionKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Click => "click",
            Self::Menu => "menu",
            Self::ScrollUp => "scroll_up",
            Self::ScrollDown => "scroll_down",
            Self::Type => "type",
            Self::PressEnter => "press_enter",
            Self::PressTab => "press_tab",
            Self::PressEscape => "press_escape",
            Self::Wait => "wait",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    Done,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JevDecision {
    pub choice: String,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
    #[serde(default)]
    pub model: Option<String>,
}
