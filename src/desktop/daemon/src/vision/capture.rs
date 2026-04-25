#[cfg(target_os = "macos")]
#[path = "capture/macos.rs"]
mod macos_impl;

#[cfg(target_os = "macos")]
pub use macos_impl::capture_screen_png;
#[cfg(target_os = "macos")]
pub(crate) use macos_impl::capture_window_png;
#[cfg(target_os = "macos")]
pub(crate) use macos_impl::default_capture_path;

#[cfg(target_os = "windows")]
use std::{
    ffi::c_void,
    path::PathBuf,
    ptr::null_mut,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use super::types::CapturedImage;
#[cfg(target_os = "windows")]
use super::types::{CapturedFrame, CapturedImage};
#[cfg(target_os = "windows")]
use desktop_core::{error::AppError, protocol::now_millis};
#[cfg(target_os = "windows")]
use image::RgbaImage;
#[cfg(target_os = "windows")]
use windows_sys::Win32::{
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
        CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
        SRCCOPY, SelectObject,
    },
    UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    },
};

#[cfg(target_os = "windows")]
pub fn capture_screen_png(out_path: Option<PathBuf>) -> Result<CapturedImage, AppError> {
    let captured = capture_with_gdi()?;

    let image_path = if let Some(path) = out_path {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                AppError::backend_unavailable(format!(
                    "failed to create capture directory {}: {err}",
                    parent.display()
                ))
            })?;
        }
        captured.image.save(&path).map_err(|err| {
            AppError::backend_unavailable(format!(
                "failed to save capture PNG {}: {err}",
                path.display()
            ))
        })?;
        Some(path)
    } else {
        None
    };

    Ok(CapturedImage {
        frame: CapturedFrame {
            snapshot_id: now_millis() as u64,
            timestamp: now_millis().to_string(),
            display_id: 0,
            width: captured.image.width(),
            height: captured.image.height(),
            scale: 1.0,
            image_path,
        },
        image: captured.image,
    })
}

#[cfg(target_os = "windows")]
pub(crate) fn capture_window_png(
    _out_path: Option<PathBuf>,
    _window_id: u32,
    _logical_bounds: Option<&desktop_core::protocol::Bounds>,
) -> Result<CapturedImage, AppError> {
    Err(AppError::backend_unavailable(
        "background window capture is supported only on macOS; switch to frontmost mode",
    ))
}

#[cfg(target_os = "windows")]
struct GdiCapture {
    image: RgbaImage,
}

#[cfg(target_os = "windows")]
fn capture_with_gdi() -> Result<GdiCapture, AppError> {
    // SAFETY: querying screen metrics has no preconditions.
    let x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };

    if width <= 0 || height <= 0 {
        return Err(AppError::backend_unavailable(
            "invalid virtual screen metrics on Windows",
        ));
    }

    // SAFETY: null HWND requests desktop DC.
    let screen_dc = unsafe { GetDC(null_mut()) };
    if screen_dc.is_null() {
        return Err(AppError::backend_unavailable(
            "GetDC returned null for desktop",
        ));
    }

    // SAFETY: valid HDC from GetDC.
    let mem_dc = unsafe { CreateCompatibleDC(screen_dc) };
    if mem_dc.is_null() {
        // SAFETY: cleanup of valid screen_dc.
        unsafe { ReleaseDC(null_mut(), screen_dc) };
        return Err(AppError::backend_unavailable(
            "CreateCompatibleDC failed on Windows",
        ));
    }

    // SAFETY: valid HDC handles and positive dimensions.
    let bitmap = unsafe { CreateCompatibleBitmap(screen_dc, width, height) };
    if bitmap.is_null() {
        // SAFETY: cleanup of valid handles.
        unsafe {
            DeleteDC(mem_dc);
            ReleaseDC(null_mut(), screen_dc);
        }
        return Err(AppError::backend_unavailable(
            "CreateCompatibleBitmap failed on Windows",
        ));
    }

    // SAFETY: select bitmap into memory DC before blit/readback.
    let previous = unsafe { SelectObject(mem_dc, bitmap as _) };
    if previous.is_null() {
        // SAFETY: cleanup of valid handles.
        unsafe {
            DeleteObject(bitmap as _);
            DeleteDC(mem_dc);
            ReleaseDC(null_mut(), screen_dc);
        }
        return Err(AppError::backend_unavailable(
            "SelectObject failed for capture bitmap",
        ));
    }

    // SAFETY: valid DCs and dimensions.
    let blt_ok = unsafe {
        BitBlt(
            mem_dc,
            0,
            0,
            width,
            height,
            screen_dc,
            x,
            y,
            SRCCOPY | CAPTUREBLT,
        )
    };
    if blt_ok == 0 {
        // SAFETY: restore + cleanup.
        unsafe {
            SelectObject(mem_dc, previous);
            DeleteObject(bitmap as _);
            DeleteDC(mem_dc);
            ReleaseDC(null_mut(), screen_dc);
        }
        return Err(AppError::backend_unavailable(
            "BitBlt failed during Windows screen capture",
        ));
    }

    let byte_len = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    let mut bgra = vec![0_u8; byte_len];

    // SAFETY: zeroed BITMAPINFO is initialized below before use.
    let mut bmi: BITMAPINFO = unsafe { std::mem::zeroed() };
    bmi.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height, // top-down
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };

    // SAFETY: bitmap is selected into mem_dc, buffer is valid for write.
    let rows_copied = unsafe {
        GetDIBits(
            mem_dc,
            bitmap,
            0,
            height as u32,
            bgra.as_mut_ptr() as *mut c_void,
            &mut bmi as *mut BITMAPINFO,
            DIB_RGB_COLORS,
        )
    };

    // SAFETY: always restore + cleanup before returning.
    unsafe {
        SelectObject(mem_dc, previous);
        DeleteObject(bitmap as _);
        DeleteDC(mem_dc);
        ReleaseDC(null_mut(), screen_dc);
    }

    if rows_copied == 0 {
        return Err(AppError::backend_unavailable(
            "GetDIBits failed during Windows screen capture",
        ));
    }

    // Convert BGRA -> RGBA in-place by swapping B and R channels.
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }

    let image = RgbaImage::from_vec(width as u32, height as u32, bgra).ok_or_else(|| {
        AppError::backend_unavailable("failed to build RGBA capture image from GDI buffer")
    })?;

    Ok(GdiCapture { image })
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
pub(crate) fn capture_window_png(
    _out_path: Option<PathBuf>,
    _window_id: u32,
    _logical_bounds: Option<&desktop_core::protocol::Bounds>,
) -> Result<CapturedImage, AppError> {
    Err(AppError::backend_unavailable(format!(
        "background window capture is unsupported on {}; switch to frontmost mode",
        std::env::consts::OS
    )))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn default_capture_path() -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    PathBuf::from(format!("/tmp/desktopctl-captures/capture-{ts}.png"))
}
