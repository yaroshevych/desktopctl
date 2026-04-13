#[cfg(target_os = "macos")]
#[path = "capture/macos.rs"]
mod macos_impl;

#[cfg(target_os = "macos")]
pub use macos_impl::capture_screen_png;
#[cfg(target_os = "macos")]
pub(crate) use macos_impl::default_capture_path;

#[cfg(target_os = "windows")]
use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "windows")]
use super::types::CapturedFrame;
#[cfg(not(target_os = "macos"))]
use super::types::CapturedImage;
#[cfg(target_os = "windows")]
use desktop_core::{error::AppError, protocol::now_millis};

#[cfg(target_os = "windows")]
pub fn capture_screen_png(out_path: Option<PathBuf>) -> Result<CapturedImage, AppError> {
    let target_path = out_path.clone().unwrap_or_else(default_capture_path);
    capture_with_powershell(&target_path)?;

    let image = image::open(&target_path)
        .map_err(|err| {
            AppError::backend_unavailable(format!(
                "failed to open captured image {}: {err}",
                target_path.display()
            ))
        })?
        .to_rgba8();

    let width = image.width();
    let height = image.height();
    let image_path = if out_path.is_some() {
        Some(target_path)
    } else {
        let _ = fs::remove_file(&target_path);
        None
    };

    Ok(CapturedImage {
        frame: CapturedFrame {
            snapshot_id: now_millis() as u64,
            timestamp: now_millis().to_string(),
            display_id: 0,
            width,
            height,
            scale: 1.0,
            image_path,
        },
        image,
    })
}

#[cfg(target_os = "windows")]
pub(crate) fn default_capture_path() -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    std::env::temp_dir()
        .join("desktopctl-captures")
        .join(format!("capture-{ts}.png"))
}

#[cfg(target_os = "windows")]
fn capture_with_powershell(path: &PathBuf) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::backend_unavailable(format!(
                "failed to create capture directory {}: {err}",
                parent.display()
            ))
        })?;
    }

    let escaped = path.display().to_string().replace('\'', "''");
    let script = format!(
        r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$bitmap = New-Object System.Drawing.Bitmap($bounds.Width, $bounds.Height)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($bounds.X, $bounds.Y, 0, 0, $bitmap.Size)
$bitmap.Save('{escaped}', [System.Drawing.Imaging.ImageFormat]::Png)
$graphics.Dispose()
$bitmap.Dispose()
"#
    );

    let output = Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run powershell: {err}")))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(AppError::backend_unavailable(format!(
        "powershell capture failed: {stderr}"
    )))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use desktop_core::error::AppError;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn capture_screen_png(_out_path: Option<PathBuf>) -> Result<CapturedImage, AppError> {
    Err(AppError::backend_unavailable(
        "screen capture backend not implemented for this platform",
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn default_capture_path() -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    PathBuf::from(format!("/tmp/desktopctl-captures/capture-{ts}.png"))
}
