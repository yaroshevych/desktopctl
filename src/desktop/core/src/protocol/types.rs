use serde::{Deserialize, Serialize, Serializer, ser::SerializeSeq};

pub const PROTOCOL_VERSION: u32 = 1;
pub const API_VERSION: &str = "1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bounds {
    #[serde(serialize_with = "serialize_compact_f64")]
    pub x: f64,
    #[serde(serialize_with = "serialize_compact_f64")]
    pub y: f64,
    #[serde(serialize_with = "serialize_compact_f64")]
    pub width: f64,
    #[serde(serialize_with = "serialize_compact_f64")]
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotText {
    pub text: String,
    pub bounds: Bounds,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotDisplay {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    #[serde(serialize_with = "serialize_compact_f64")]
    pub scale: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotPayload {
    pub snapshot_id: u64,
    pub timestamp: String,
    pub display: SnapshotDisplay,
    pub focused_app: Option<String>,
    pub texts: Vec<SnapshotText>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenEntry {
    pub n: u32,
    pub text: String,
    pub bounds: Bounds,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizePayload {
    pub snapshot_id: u64,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<TokenizeImage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<TokenizeWindow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizeImage {
    pub path: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizeWindow {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_ref: Option<String>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    pub bounds: Bounds,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_bounds: Option<Bounds>,
    pub elements: Vec<TokenizeElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizeElement {
    pub id: String,
    #[serde(rename = "type", default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(serialize_with = "serialize_compact_bbox")]
    pub bbox: [f64; 4],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_border: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrollable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<ToggleState>,
    pub source: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToggleState {
    True,
    False,
    Mixed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionState {
    pub granted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionsPayload {
    pub accessibility: PermissionState,
    pub screen_recording: PermissionState,
}

fn serialize_compact_f64<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if value.is_finite() && value.fract() == 0.0 {
        let int = *value as i64;
        if (int as f64) == *value {
            return serializer.serialize_i64(int);
        }
    }
    serializer.serialize_f64(*value)
}

fn serialize_compact_bbox<S>(bbox: &[f64; 4], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut seq = serializer.serialize_seq(Some(4))?;
    for value in bbox {
        seq.serialize_element(&CompactF64(*value))?;
    }
    seq.end()
}

struct CompactF64(f64);

impl Serialize for CompactF64 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_compact_f64(&self.0, serializer)
    }
}

pub fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
