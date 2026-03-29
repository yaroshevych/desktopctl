use serde::{Deserialize, Serialize};

const DEFAULT_OBSERVE_TIMEOUT_MS: u64 = 300;
const DEFAULT_OBSERVE_SETTLE_MS: u64 = 650;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObserveUntil {
    Stable,
    Change,
    FirstChange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveOptions {
    pub enabled: bool,
    pub until: ObserveUntil,
    pub timeout_ms: u64,
    #[serde(default = "default_observe_settle_ms")]
    pub settle_ms: u64,
    #[serde(default)]
    pub save_crops: bool,
}

impl Default for ObserveOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            until: ObserveUntil::Stable,
            timeout_ms: DEFAULT_OBSERVE_TIMEOUT_MS,
            settle_ms: DEFAULT_OBSERVE_SETTLE_MS,
            save_crops: false,
        }
    }
}

fn default_observe_settle_ms() -> u64 {
    DEFAULT_OBSERVE_SETTLE_MS
}
