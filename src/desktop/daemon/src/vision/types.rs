use std::path::PathBuf;

use image::RgbaImage;

#[derive(Debug, Clone)]
pub struct CapturedFrame {
    #[allow(dead_code)]
    pub snapshot_id: u64,
    pub timestamp: String,
    pub display_id: u32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub image_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct CapturedImage {
    pub frame: CapturedFrame,
    pub image: RgbaImage,
}
