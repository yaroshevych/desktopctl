#[cfg(target_os = "macos")]
#[path = "ocr/macos.rs"]
mod macos_impl;

#[cfg(any(all(target_os = "windows", target_env = "msvc"), target_os = "windows"))]
use desktop_core::protocol::Bounds;
use desktop_core::{error::AppError, protocol::SnapshotText};
use image::RgbaImage;

#[cfg(all(target_os = "windows", target_env = "msvc"))]
use leptess::LepTess;

#[cfg(target_os = "macos")]
#[allow(dead_code)]
pub fn recognize_text(image: &RgbaImage) -> Result<Vec<SnapshotText>, AppError> {
    macos_impl::recognize_text(image)
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
pub fn recognize_text_from_image(
    path: &std::path::Path,
    image_width: u32,
    image_height: u32,
) -> Result<Vec<SnapshotText>, AppError> {
    macos_impl::recognize_text_from_image(path, image_width, image_height)
}

#[cfg(all(target_os = "windows", target_env = "msvc"))]
pub fn recognize_text(image: &RgbaImage) -> Result<Vec<SnapshotText>, AppError> {
    let mut encoded = Vec::new();
    {
        let encoder = image::codecs::png::PngEncoder::new(&mut encoded);
        use image::ImageEncoder as _;
        encoder
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                image::ColorType::Rgba8.into(),
            )
            .map_err(|err| {
                AppError::backend_unavailable(format!("failed to encode OCR input PNG: {err}"))
            })?;
    }

    let mut lt = LepTess::new(None, "eng").map_err(|err| {
        AppError::backend_unavailable(format!(
            "failed to initialize Tesseract engine: {err}. ensure Tesseract + tessdata (eng) are installed"
        ))
    })?;
    lt.set_image_from_mem(&encoded).map_err(|err| {
        AppError::backend_unavailable(format!("failed to load OCR image into Tesseract: {err}"))
    })?;

    let tsv = lt.get_tsv_text(0).map_err(|err| {
        AppError::backend_unavailable(format!("failed to retrieve OCR TSV output: {err}"))
    })?;

    parse_tesseract_tsv(&tsv, image.width(), image.height())
}

#[cfg(all(target_os = "windows", target_env = "msvc"))]
#[allow(dead_code)]
pub fn recognize_text_from_image(
    path: &std::path::Path,
    image_width: u32,
    image_height: u32,
) -> Result<Vec<SnapshotText>, AppError> {
    let (file_width, file_height) = image::image_dimensions(path).map_err(|err| {
        AppError::invalid_argument(format!(
            "failed to read image dimensions {}: {err}",
            path.display()
        ))
    })?;

    let bytes = std::fs::read(path).map_err(|err| {
        AppError::invalid_argument(format!("failed to read image {}: {err}", path.display()))
    })?;

    let mut lt = LepTess::new(None, "eng").map_err(|err| {
        AppError::backend_unavailable(format!(
            "failed to initialize Tesseract engine: {err}. ensure Tesseract + tessdata (eng) are installed"
        ))
    })?;
    lt.set_image_from_mem(&bytes).map_err(|err| {
        AppError::backend_unavailable(format!("failed to load OCR image into Tesseract: {err}"))
    })?;
    let tsv = lt.get_tsv_text(0).map_err(|err| {
        AppError::backend_unavailable(format!("failed to retrieve OCR TSV output: {err}"))
    })?;

    parse_tesseract_tsv(
        &tsv,
        if image_width == 0 {
            file_width
        } else {
            image_width
        },
        if image_height == 0 {
            file_height
        } else {
            image_height
        },
    )
}

#[cfg(all(target_os = "windows", not(target_env = "msvc")))]
pub fn recognize_text(image: &RgbaImage) -> Result<Vec<SnapshotText>, AppError> {
    let tmp_path = temp_input_path("ocr-input", "png");
    image.save(&tmp_path).map_err(|err| {
        AppError::backend_unavailable(format!(
            "failed to write temporary OCR image {}: {err}",
            tmp_path.display()
        ))
    })?;

    let output = tesseract_command()
        .arg(&tmp_path)
        .arg("stdout")
        .arg("tsv")
        .output()
        .map_err(|err| {
            AppError::backend_unavailable(format!(
                "failed to run tesseract binary: {err}. install Tesseract, set DESKTOPCTL_TESSERACT_BIN, or provide it on PATH"
            ))
        })?;
    let _ = std::fs::remove_file(&tmp_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::backend_unavailable(format!(
            "tesseract OCR failed: {stderr}"
        )));
    }

    parse_tesseract_tsv(
        &String::from_utf8_lossy(&output.stdout),
        image.width(),
        image.height(),
    )
}

#[cfg(all(target_os = "windows", not(target_env = "msvc")))]
#[allow(dead_code)]
pub fn recognize_text_from_image(
    path: &std::path::Path,
    image_width: u32,
    image_height: u32,
) -> Result<Vec<SnapshotText>, AppError> {
    let (file_width, file_height) = image::image_dimensions(path).map_err(|err| {
        AppError::invalid_argument(format!(
            "failed to read image dimensions {}: {err}",
            path.display()
        ))
    })?;

    parse_tesseract_tsv(
        &run_tesseract_tsv(path)?,
        if image_width == 0 {
            file_width
        } else {
            image_width
        },
        if image_height == 0 {
            file_height
        } else {
            image_height
        },
    )
}

#[cfg(all(target_os = "windows", not(target_env = "msvc")))]
fn run_tesseract_tsv(path: &std::path::Path) -> Result<String, AppError> {
    let output = tesseract_command()
        .arg(path)
        .arg("stdout")
        .arg("tsv")
        .output()
        .map_err(|err| {
            AppError::backend_unavailable(format!(
                "failed to run tesseract binary: {err}. install Tesseract, set DESKTOPCTL_TESSERACT_BIN, or provide it on PATH"
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::backend_unavailable(format!(
            "tesseract OCR failed: {stderr}"
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(all(target_os = "windows", not(target_env = "msvc")))]
fn tesseract_command() -> std::process::Command {
    if let Some(path) = std::env::var("DESKTOPCTL_TESSERACT_BIN")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        return std::process::Command::new(path);
    }
    if let Some(path) = std::env::var("TESSERACT_PATH")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        return std::process::Command::new(path);
    }
    std::process::Command::new("tesseract")
}

#[cfg(target_os = "windows")]
fn parse_tesseract_tsv(
    tsv: &str,
    image_width: u32,
    image_height: u32,
) -> Result<Vec<SnapshotText>, AppError> {
    let mut rows = Vec::new();
    for (line_index, line) in tsv.lines().enumerate() {
        if line_index == 0 || line.trim().is_empty() {
            continue;
        }
        let columns: Vec<&str> = line.split('\t').collect();
        if columns.len() < 12 {
            continue;
        }

        let text = columns[11].trim();
        if text.is_empty() {
            continue;
        }

        let confidence = match columns[10].trim().parse::<f32>() {
            Ok(value) if value >= 0.0 => (value / 100.0).clamp(0.0, 1.0),
            _ => continue,
        };

        let left = match columns[6].trim().parse::<f64>() {
            Ok(value) => value.max(0.0),
            Err(_) => continue,
        };
        let top = match columns[7].trim().parse::<f64>() {
            Ok(value) => value.max(0.0),
            Err(_) => continue,
        };
        let width = match columns[8].trim().parse::<f64>() {
            Ok(value) => value.max(0.0),
            Err(_) => continue,
        };
        let height = match columns[9].trim().parse::<f64>() {
            Ok(value) => value.max(0.0),
            Err(_) => continue,
        };

        rows.push(SnapshotText {
            text: text.to_string(),
            bounds: Bounds {
                x: left.min(image_width as f64),
                y: top.min(image_height as f64),
                width,
                height,
            },
            confidence,
        });
    }

    Ok(rows)
}

#[cfg(all(target_os = "windows", not(target_env = "msvc")))]
fn temp_input_path(prefix: &str, ext: &str) -> std::path::PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("desktopctl-{prefix}-{ts}.{ext}"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn recognize_text(_image: &RgbaImage) -> Result<Vec<SnapshotText>, AppError> {
    Ok(Vec::new())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[allow(dead_code)]
pub fn recognize_text_from_image(
    _path: &std::path::Path,
    _image_width: u32,
    _image_height: u32,
) -> Result<Vec<SnapshotText>, AppError> {
    Ok(Vec::new())
}
