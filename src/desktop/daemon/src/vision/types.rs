use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedFrame {
    pub snapshot_id: u64,
    pub timestamp: String,
    pub display_id: u32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub image_path: PathBuf,
}
