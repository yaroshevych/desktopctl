//! PipeWire frame ingestion for ScreenCast capture (DesktopCtl-k7n).
//!
//! Given the PipeWire remote fd + node id from [`super::portal`], build a
//! PipeWire input stream, negotiate a raw `Video` format (RGBx/BGRx), run the
//! main loop just long enough to pull ONE frame, and copy it into an owned
//! RGBA8 buffer.
//!
//! The daemon is synchronous and PipeWire's `MainLoop` is itself a blocking
//! run loop, so this runs the loop inline (no tokio). A timer bounds the wait
//! so a stalled negotiation cannot hang the daemon.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use desktop_core::error::AppError;
use serde_json::json;

use pipewire as pw;
use pw::{properties::properties, spa};
use spa::param::video::{VideoFormat, VideoInfoRaw};

use super::portal::ScreenCastSession;

/// Maximum time to wait for the first frame before giving up.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// One captured frame in RGBA8, row-major, tightly packed (no padding).
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    /// RGBA8 pixels: `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

/// Shared state between the PipeWire callbacks and the caller.
#[derive(Default)]
struct CaptureState {
    /// Negotiated video format, set in `param_changed`.
    format: VideoInfoRaw,
    /// `true` once a format has been negotiated.
    have_format: bool,
    /// The captured frame, set in `process`.
    frame: Option<CapturedFrame>,
    /// A terminal stream error reported via `state_changed`.
    error: Option<String>,
    /// Set when the timeout fired before a frame arrived.
    timed_out: bool,
}

/// Capture a single frame from the session's primary monitor stream.
pub fn capture_one(session: &ScreenCastSession) -> Result<CapturedFrame, AppError> {
    let node_id = session.primary_node_id()?;
    let fd = session.pipewire_fd_cloned()?;

    pw::init();

    let mainloop = pw::main_loop::MainLoop::new(None).map_err(pw_err)?;
    let context = pw::context::Context::new(&mainloop).map_err(pw_err)?;
    let core = context.connect_fd(fd, None).map_err(pw_err)?;

    let state = Rc::new(RefCell::new(CaptureState::default()));

    let stream = pw::stream::Stream::new(
        &core,
        "desktopctl-capture",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(pw_err)?;

    // --- Callbacks ---------------------------------------------------------
    // Each closure is `'static`, so shared state is cloned `Rc`s and the loop
    // is quit via a weak handle to avoid a reference cycle.

    // `WeakMainLoop` is not `Clone`, so build one weak handle per callback.
    let state_sc = state.clone();
    let loop_sc = mainloop.downgrade();
    let state_pc = state.clone();
    let state_proc = state.clone();
    let loop_proc = mainloop.downgrade();

    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_stream, _ud, _old, new| {
            if let pw::stream::StreamState::Error(msg) = new {
                state_sc.borrow_mut().error = Some(msg.clone());
                if let Some(l) = loop_sc.upgrade() {
                    l.quit();
                }
            }
        })
        .param_changed(move |_stream, _ud, id, param| {
            let Some(param) = param else { return };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = pw::spa::param::format_utils::parse_format(param)
            else {
                return;
            };
            if media_type != pw::spa::param::format::MediaType::Video
                || media_subtype != pw::spa::param::format::MediaSubtype::Raw
            {
                return;
            }
            let mut st = state_pc.borrow_mut();
            if st.format.parse(param).is_ok() {
                st.have_format = true;
            }
        })
        .process(move |stream, _ud| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }

            // Extract format/dimensions before borrowing buffer data mutably.
            let (fmt, width, height) = {
                let st = state_proc.borrow();
                if !st.have_format {
                    return;
                }
                let size = st.format.size();
                (st.format.format(), size.width, size.height)
            };
            if width == 0 || height == 0 {
                return;
            }

            let data = &mut datas[0];
            let stride = data.chunk().stride().max(0) as usize;
            let chunk_size = data.chunk().size() as usize;
            let Some(bytes) = data.data() else { return };
            if bytes.is_empty() || chunk_size == 0 {
                return;
            }

            match convert_to_rgba(bytes, fmt, width, height, stride) {
                Some(frame) => {
                    let mut st = state_proc.borrow_mut();
                    if st.frame.is_none() {
                        st.frame = Some(frame);
                    }
                }
                None => {
                    let mut st = state_proc.borrow_mut();
                    if st.error.is_none() {
                        st.error = Some(format!("unsupported video format {fmt:?}"));
                    }
                }
            }

            // We have what we need (or hit an error); stop the loop.
            if let Some(l) = loop_proc.upgrade() {
                l.quit();
            }
        })
        .register()
        .map_err(pw_err)?;

    // --- Format negotiation POD --------------------------------------------
    // Offer RGBx/BGRx (4 bytes/pixel) which we can copy to RGBA directly.
    let obj = pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamFormat,
        pw::spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaType,
            Id,
            pw::spa::param::format::MediaType::Video
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            pw::spa::param::format::MediaSubtype::Raw
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            // default + alternatives
            pw::spa::param::video::VideoFormat::RGBx,
            pw::spa::param::video::VideoFormat::RGBx,
            pw::spa::param::video::VideoFormat::BGRx,
            pw::spa::param::video::VideoFormat::RGBA,
            pw::spa::param::video::VideoFormat::BGRA,
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            pw::spa::utils::Rectangle {
                width: 1920,
                height: 1080
            },
            pw::spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            pw::spa::utils::Rectangle {
                width: 8192,
                height: 8192
            }
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            pw::spa::utils::Fraction { num: 30, denom: 1 },
            pw::spa::utils::Fraction { num: 0, denom: 1 },
            pw::spa::utils::Fraction {
                num: 1000,
                denom: 1
            }
        ),
    );

    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )
    .map_err(|e| stream_failed(format!("failed to serialize format POD: {e}")))?
    .0
    .into_inner();

    let pod =
        spa::pod::Pod::from_bytes(&values).ok_or_else(|| stream_failed("invalid format POD"))?;
    let mut params = [pod];

    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(pw_err)?;

    // --- Bounded run -------------------------------------------------------
    // Arm a one-shot timer that quits the loop if no frame arrives in time.
    let loop_timer = mainloop.downgrade();
    let state_timer = state.clone();
    let timer = mainloop.loop_().add_timer(move |_expirations| {
        state_timer.borrow_mut().timed_out = true;
        if let Some(l) = loop_timer.upgrade() {
            l.quit();
        }
    });
    timer
        .update_timer(Some(FRAME_TIMEOUT), None)
        .into_result()
        .map_err(|e| stream_failed(format!("failed to arm capture timer: {e:?}")))?;

    mainloop.run();

    // --- Result ------------------------------------------------------------
    let mut st = state.borrow_mut();
    if let Some(err) = st.error.take() {
        return Err(stream_failed(format!("PipeWire stream error: {err}")));
    }
    if let Some(frame) = st.frame.take() {
        return Ok(frame);
    }
    if st.timed_out {
        return Err(
            AppError::timeout("timed out waiting for first PipeWire capture frame")
                .with_details(json!({ "failure_state": "pipewire_stream_failed" })),
        );
    }
    Err(stream_failed("PipeWire stream produced no frame"))
}

/// Convert a captured 4-bytes-per-pixel frame to tightly packed RGBA8.
///
/// `stride` is the source row stride in bytes (may include padding). Returns
/// `None` for formats we did not negotiate.
fn convert_to_rgba(
    src: &[u8],
    fmt: VideoFormat,
    width: u32,
    height: u32,
    stride: usize,
) -> Option<CapturedFrame> {
    let w = width as usize;
    let h = height as usize;
    let row_bytes = w.checked_mul(4)?;
    // If the chunk reports no usable stride, assume tightly packed rows.
    let src_stride = if stride >= row_bytes {
        stride
    } else {
        row_bytes
    };

    // Per-pixel byte permutation from source -> RGBA.
    // Source layouts are little-endian 32-bit "x" formats where the X byte is
    // ignored; we force alpha to opaque.
    let swap_rb = match fmt {
        VideoFormat::RGBx | VideoFormat::RGBA => false,
        VideoFormat::BGRx | VideoFormat::BGRA => true,
        _ => return None,
    };

    let mut pixels = vec![0u8; row_bytes.checked_mul(h)?];
    for y in 0..h {
        let src_row_start = y.checked_mul(src_stride)?;
        let src_row_end = src_row_start.checked_add(row_bytes)?;
        if src_row_end > src.len() {
            // Source buffer shorter than expected; bail rather than read OOB.
            return None;
        }
        let src_row = &src[src_row_start..src_row_end];
        let dst_row = &mut pixels[y * row_bytes..(y + 1) * row_bytes];
        for x in 0..w {
            let s = &src_row[x * 4..x * 4 + 4];
            let d = &mut dst_row[x * 4..x * 4 + 4];
            if swap_rb {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
            } else {
                d[0] = s[0];
                d[1] = s[1];
                d[2] = s[2];
            }
            d[3] = 255; // opaque; "x"/alpha byte from source is ignored
        }
    }

    Some(CapturedFrame {
        width,
        height,
        pixels,
    })
}

fn pw_err(e: pw::Error) -> AppError {
    stream_failed(format!("PipeWire error: {e}"))
}

fn stream_failed(msg: impl Into<String>) -> AppError {
    AppError::backend_unavailable(msg)
        .with_details(json!({ "failure_state": "pipewire_stream_failed" }))
}
