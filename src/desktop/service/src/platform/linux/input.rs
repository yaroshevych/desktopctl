//! RemoteDesktop + EIS (reis) pointer/keyboard input emission (DesktopCtl-91v).
//! Implemented by the input/coordinate agent.
//!
//! See `tmp/ubuntu-spec.md`, sections "Input" and "Coordinate Model".
//!
//! ## Design
//!
//! The daemon is **synchronous** (no tokio). `ashpd` is async-only, so every
//! portal call is driven across a [`futures_lite::future::block_on`] boundary;
//! no public method on [`InputBackend`] is async.
//!
//! Input is delivered through `org.freedesktop.portal.RemoteDesktop`:
//!
//! 1. `CreateSession`
//! 2. `SelectDevices` (pointer + keyboard)
//! 3. `Start` (user consent dialog)
//! 4. `ConnectToEIS` → an EIS file descriptor, wrapped in a `reis::ei::Context`
//!    whose handshake is initiated immediately.
//!
//! Pointer / keyboard / scroll events are emitted via the portal's
//! `Notify*` methods, which the compositor forwards over libei/EIS. The raw
//! EIS context obtained from `ConnectToEIS` is retained for the direct-`reis`
//! emission path; the per-device EIS negotiation (seat → device → keymap, all
//! delivered as asynchronous EIS events that require an event loop) is the one
//! piece left as a clearly-marked TODO — see [`InputBackend::emit_via_eis`].
//! The portal `Notify*` path is fully functional on its own.
//!
//! Key/text emission resolves characters → keysyms → keycodes through
//! `xkbcommon`, honouring the active keymap (the spec requires respecting
//! libei keymaps; we build one from the default RMLVO names, replaced by the
//! EIS-provided keymap once device negotiation lands).

use std::os::unix::net::UnixStream;

use ashpd::desktop::{
    PersistMode, Session,
    remote_desktop::{Axis, DeviceType, KeyState, RemoteDesktop},
};
use desktop_core::error::{AppError, ErrorCode};
use futures_lite::future::block_on;
use reis::ei;
use serde_json::json;
use xkbcommon::xkb;

/// Linux evdev button codes (see `<linux/input-event-codes.h>`).
pub const BTN_LEFT: i32 = 0x110;
pub const BTN_RIGHT: i32 = 0x111;
pub const BTN_MIDDLE: i32 = 0x112;

/// xkb keycodes are offset by 8 from evdev/libei keycodes.
const XKB_EVDEV_OFFSET: u32 = 8;

/// A keyboard modifier usable in [`InputBackend::hotkey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Shift,
    Ctrl,
    Alt,
    Super,
}

impl Modifier {
    fn keysym(self) -> xkb::Keysym {
        match self {
            Modifier::Shift => xkb::Keysym::Shift_L,
            Modifier::Ctrl => xkb::Keysym::Control_L,
            Modifier::Alt => xkb::Keysym::Alt_L,
            Modifier::Super => xkb::Keysym::Super_L,
        }
    }
}

/// Map a spec failure-state name onto an [`AppError`] carrying
/// `details.failure_state` so downstream consumers can match the Wayland
/// failure taxonomy from the spec.
fn fail(code: ErrorCode, failure_state: &str, message: impl Into<String>) -> AppError {
    AppError::new(code, message).with_details(json!({ "failure_state": failure_state }))
}

fn device_unavailable(message: impl Into<String>) -> AppError {
    fail(
        ErrorCode::BackendUnavailable,
        "input_device_unavailable",
        message,
    )
    .with_retryable(true)
}

fn portal_unavailable(message: impl Into<String>) -> AppError {
    fail(ErrorCode::BackendUnavailable, "portal_unavailable", message).with_retryable(true)
}

fn permission_denied(message: impl Into<String>) -> AppError {
    fail(ErrorCode::PermissionDenied, "permission_denied", message)
}

fn coordinate_mapping_unavailable(message: impl Into<String>) -> AppError {
    fail(
        ErrorCode::Internal,
        "coordinate_mapping_unavailable",
        message,
    )
}

/// Translate an ashpd portal error into the spec failure taxonomy. The portal
/// surfaces user denial / cancellation through `Response`/`Portal` errors;
/// version mismatches and missing services map to portal-unavailable.
fn map_portal_error(context: &str, err: ashpd::Error) -> AppError {
    match err {
        ashpd::Error::Response(_) => {
            permission_denied(format!("{context}: portal request denied or cancelled"))
        }
        ashpd::Error::Portal(ref e) => permission_denied(format!("{context}: portal error: {e}")),
        ashpd::Error::RequiresVersion(want, have) => portal_unavailable(format!(
            "{context}: RemoteDesktop portal v{want} required, have v{have}"
        )),
        ashpd::Error::PortalNotFound(_) => {
            portal_unavailable(format!("{context}: RemoteDesktop portal not found"))
        }
        other => portal_unavailable(format!("{context}: {other}")),
    }
}

/// Synchronous wrapper around the RemoteDesktop portal + EIS input pipeline.
///
/// Holds the portal proxy, the open `Session`, the EIS context (from
/// `ConnectToEIS`), and an xkb keymap for key resolution. The struct is
/// constructed once at daemon startup (after the user has granted consent) and
/// reused for the daemon's lifetime.
pub struct InputBackend {
    proxy: RemoteDesktop<'static>,
    session: Session<'static, RemoteDesktop<'static>>,
    /// EIS context obtained from `ConnectToEIS`; the handshake has been
    /// initiated. Retained for the direct reis emission path (see
    /// [`InputBackend::emit_via_eis`]). `None` if the portal predates v2 and
    /// `ConnectToEIS` was unavailable; the `Notify*` path still works.
    eis: Option<ei::Context>,
    /// xkb keymap used to resolve characters/keysyms to keycodes.
    keymap: xkb::Keymap,
    /// Whether keyboard access was actually granted by the portal.
    has_keyboard: bool,
    /// Whether pointer access was actually granted by the portal.
    has_pointer: bool,
    /// First capture stream id, if a screencast stream was negotiated on the
    /// same session; required for absolute pointer motion (which is relative
    /// to a stream's logical coordinate space).
    pointer_stream: Option<u32>,
}

impl InputBackend {
    /// Create the RemoteDesktop session, select pointer + keyboard devices,
    /// `Start` (prompting the user), and connect the EIS endpoint.
    ///
    /// Blocks on the async portal calls. `restore_token` may be supplied to
    /// attempt promptless restoration of a previously granted session.
    pub fn new(restore_token: Option<&str>) -> Result<Self, AppError> {
        block_on(Self::setup(restore_token))
    }

    async fn setup(restore_token: Option<&str>) -> Result<Self, AppError> {
        let proxy = RemoteDesktop::new()
            .await
            .map_err(|e| map_portal_error("RemoteDesktop::new", e))?;

        let session = proxy
            .create_session()
            .await
            .map_err(|e| map_portal_error("create_session", e))?;

        proxy
            .select_devices(
                &session,
                DeviceType::Pointer | DeviceType::Keyboard,
                restore_token,
                // Persistent across sessions per the spec's restore-token flow;
                // a fresh restore token is returned on each grant.
                PersistMode::ExplicitlyRevoked,
            )
            .await
            .map_err(|e| map_portal_error("select_devices", e))?;

        let response = proxy
            .start(&session, None)
            .await
            .map_err(|e| map_portal_error("start", e))?
            .response()
            .map_err(|e| permission_denied(format!("start: no device selection returned: {e}")))?;

        let granted = response.devices();
        let has_keyboard = granted.contains(DeviceType::Keyboard);
        let has_pointer = granted.contains(DeviceType::Pointer);
        if !has_keyboard && !has_pointer {
            return Err(device_unavailable(
                "RemoteDesktop granted neither pointer nor keyboard",
            ));
        }

        let pointer_stream = response
            .streams()
            .and_then(|streams| streams.first())
            .map(|s| s.pipe_wire_node_id());

        // ConnectToEIS requires portal v2. Treat its absence as non-fatal: the
        // Notify* path still delivers input. A hard error here would make the
        // backend unusable on older portals.
        let eis = match proxy.connect_to_eis(&session).await {
            Ok(fd) => {
                let stream = UnixStream::from(fd);
                match ei::Context::new(stream) {
                    Ok(ctx) => {
                        // Kick off the EIS handshake. Full per-device
                        // negotiation is driven in emit_via_eis (TODO).
                        let _handshake = ctx.handshake();
                        let _ = ctx.flush();
                        Some(ctx)
                    }
                    Err(_) => None,
                }
            }
            Err(_) => None,
        };

        let keymap = default_keymap()
            .ok_or_else(|| device_unavailable("failed to compile default xkb keymap"))?;

        Ok(Self {
            proxy,
            session,
            eis,
            keymap,
            has_keyboard,
            has_pointer,
            pointer_stream,
        })
    }

    fn ensure_pointer(&self) -> Result<(), AppError> {
        if self.has_pointer {
            Ok(())
        } else {
            Err(device_unavailable("pointer device was not granted"))
        }
    }

    fn ensure_keyboard(&self) -> Result<(), AppError> {
        if self.has_keyboard {
            Ok(())
        } else {
            Err(device_unavailable("keyboard device was not granted"))
        }
    }

    // --- pointer ------------------------------------------------------------

    /// Move the pointer by a relative delta in the stream's logical space.
    pub fn pointer_move_relative(&self, dx: f64, dy: f64) -> Result<(), AppError> {
        self.ensure_pointer()?;
        block_on(self.proxy.notify_pointer_motion(&self.session, dx, dy))
            .map_err(|e| map_portal_error("notify_pointer_motion", e))
    }

    /// Move the pointer to an absolute position within the capture stream's
    /// logical coordinate space. Requires a negotiated capture stream.
    pub fn pointer_move_absolute(&self, x: f64, y: f64) -> Result<(), AppError> {
        self.ensure_pointer()?;
        let stream = self.pointer_stream.ok_or_else(|| {
            coordinate_mapping_unavailable(
                "absolute pointer motion requires a capture stream on the session",
            )
        })?;
        block_on(
            self.proxy
                .notify_pointer_motion_absolute(&self.session, stream, x, y),
        )
        .map_err(|e| map_portal_error("notify_pointer_motion_absolute", e))
    }

    /// Press or release a pointer button (evdev button code, e.g. [`BTN_LEFT`]).
    pub fn button(&self, code: i32, press: bool) -> Result<(), AppError> {
        self.ensure_pointer()?;
        let state = key_state(press);
        block_on(self.proxy.notify_pointer_button(&self.session, code, state))
            .map_err(|e| map_portal_error("notify_pointer_button", e))
    }

    /// Smooth scroll by `(dx, dy)` (touchpad-style continuous deltas).
    pub fn scroll(&self, dx: f64, dy: f64) -> Result<(), AppError> {
        self.ensure_pointer()?;
        block_on(self.proxy.notify_pointer_axis(&self.session, dx, dy, true))
            .map_err(|e| map_portal_error("notify_pointer_axis", e))
    }

    /// Discrete (wheel-step) scroll. `steps` is the number of clicks; positive
    /// vertical is down, positive horizontal is right (libei convention).
    pub fn scroll_discrete(&self, axis_horizontal: bool, steps: i32) -> Result<(), AppError> {
        self.ensure_pointer()?;
        let axis = if axis_horizontal {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        block_on(
            self.proxy
                .notify_pointer_axis_discrete(&self.session, axis, steps),
        )
        .map_err(|e| map_portal_error("notify_pointer_axis_discrete", e))
    }

    // --- keyboard -----------------------------------------------------------

    /// Press or release a raw keycode. `keycode` is an evdev/libei keycode
    /// (i.e. xkb keycode minus 8), matching the portal's `NotifyKeyboardKeycode`
    /// contract.
    pub fn key(&self, keycode: i32, press: bool) -> Result<(), AppError> {
        self.ensure_keyboard()?;
        block_on(
            self.proxy
                .notify_keyboard_keycode(&self.session, keycode, key_state(press)),
        )
        .map_err(|e| map_portal_error("notify_keyboard_keycode", e))
    }

    /// Type a UTF-8 string by resolving each character to a keysym, then to a
    /// keycode (+ shift level) via the active xkb keymap, and emitting
    /// press/release pairs. Characters with no mapping in the current layout
    /// fall back to `NotifyKeyboardKeysym` so they still get delivered.
    pub fn type_text(&self, text: &str) -> Result<(), AppError> {
        self.ensure_keyboard()?;
        for ch in text.chars() {
            let keysym = xkb::Keysym::from_char(ch);
            match self.resolve_keysym(keysym) {
                Some((keycode, shift)) => self.tap_keycode(keycode, shift)?,
                None => self.tap_keysym(keysym)?,
            }
        }
        Ok(())
    }

    /// Emit a modifier hotkey: hold each modifier, tap `key`, then release the
    /// modifiers in reverse. `key` is matched against the keymap by keysym
    /// name (e.g. `"c"`, `"Return"`, `"F5"`).
    pub fn hotkey(&self, modifiers: &[Modifier], key: &str) -> Result<(), AppError> {
        self.ensure_keyboard()?;
        let mut keysym = xkb::keysym_from_name(key, xkb::KEYSYM_NO_FLAGS);
        if keysym == xkb::Keysym::NoSymbol {
            // Try a single-char interpretation if it is not a named keysym.
            let mut chars = key.chars();
            keysym = match (chars.next(), chars.next()) {
                (Some(c), None) => xkb::Keysym::from_char(c),
                _ => {
                    return Err(fail(
                        ErrorCode::InvalidArgument,
                        "coordinate_mapping_unavailable",
                        format!("unknown hotkey key: {key:?}"),
                    ));
                }
            };
        }

        let (keycode, key_shift) = self.resolve_keysym(keysym).ok_or_else(|| {
            device_unavailable(format!(
                "no keycode for hotkey key {key:?} in active layout"
            ))
        })?;

        // Resolve modifier keycodes up front so a failure does not leave keys
        // stuck down.
        let mut mod_codes = Vec::with_capacity(modifiers.len());
        for m in modifiers {
            let (mc, _) = self
                .resolve_keysym(m.keysym())
                .ok_or_else(|| device_unavailable(format!("no keycode for modifier {m:?}")))?;
            mod_codes.push(mc);
        }

        for mc in &mod_codes {
            self.key(*mc as i32, true)?;
        }
        let res = self.tap_keycode(keycode, key_shift);
        // Always release modifiers, even if the inner tap failed.
        for mc in mod_codes.iter().rev() {
            let _ = self.key(*mc as i32, false);
        }
        res
    }

    // --- key resolution helpers --------------------------------------------

    /// Resolve a keysym to an evdev keycode and whether shift is required,
    /// scanning the active keymap. Returns the evdev keycode (xkb keycode - 8).
    fn resolve_keysym(&self, target: xkb::Keysym) -> Option<(u32, bool)> {
        let min = self.keymap.min_keycode().raw();
        let max = self.keymap.max_keycode().raw();
        for kc in min..=max {
            // Level 0 (unshifted) and level 1 (shifted) on layout 0.
            for level in 0..=1u32 {
                let syms = self
                    .keymap
                    .key_get_syms_by_level(xkb::Keycode::new(kc), 0, level);
                if syms.contains(&target) {
                    return Some((kc - XKB_EVDEV_OFFSET, level == 1));
                }
            }
        }
        None
    }

    /// Press (optionally with shift) and release a single evdev keycode.
    fn tap_keycode(&self, keycode: u32, shift: bool) -> Result<(), AppError> {
        let shift_code = if shift {
            Some(
                self.resolve_keysym(xkb::Keysym::Shift_L)
                    .map(|(c, _)| c)
                    .ok_or_else(|| device_unavailable("no Shift_L keycode in layout"))?,
            )
        } else {
            None
        };
        if let Some(sc) = shift_code {
            self.key(sc as i32, true)?;
        }
        self.key(keycode as i32, true)?;
        self.key(keycode as i32, false)?;
        if let Some(sc) = shift_code {
            self.key(sc as i32, false)?;
        }
        Ok(())
    }

    /// Fallback for characters with no keycode in the active layout: press and
    /// release the keysym directly via the portal.
    fn tap_keysym(&self, keysym: xkb::Keysym) -> Result<(), AppError> {
        let raw = keysym.raw() as i32;
        block_on(
            self.proxy
                .notify_keyboard_keysym(&self.session, raw, KeyState::Pressed),
        )
        .map_err(|e| map_portal_error("notify_keyboard_keysym", e))?;
        block_on(
            self.proxy
                .notify_keyboard_keysym(&self.session, raw, KeyState::Released),
        )
        .map_err(|e| map_portal_error("notify_keyboard_keysym", e))
    }

    // --- direct EIS path (stubbed) -----------------------------------------

    /// Direct EIS emission via `reis`, bypassing the portal `Notify*` methods.
    ///
    /// TODO(DesktopCtl-91v): the EIS protocol negotiates seats, devices and
    /// keymaps through asynchronous events that must be pumped on an event loop
    /// (`reis::calloop` or a hand-rolled `Context::read`/`pending_event` loop;
    /// see the `reis` `type-text` example). Until that negotiation is
    /// implemented we route all emission through the portal `Notify*` methods,
    /// which the compositor forwards over the same libei pipeline. The
    /// handshake is already initiated in `setup`; this hook reserves the
    /// integration point and currently performs a non-blocking drain so pending
    /// EIS events do not accumulate on the socket.
    #[allow(dead_code)]
    fn emit_via_eis(&self) -> Result<(), AppError> {
        if let Some(ctx) = &self.eis {
            // Drain any pending protocol events without blocking; full device
            // negotiation + reis::ei::{Pointer,Keyboard} emission is pending.
            let _ = ctx.read();
            while let Some(_event) = ctx.pending_event() {
                // TODO: handle Handshake/Seat/Device/Keymap events, bind the
                // keyboard + pointer capabilities, and emit via the bound
                // reis::ei device interfaces.
            }
            let _ = ctx.flush();
        }
        Ok(())
    }

    /// Close the RemoteDesktop session. Best-effort; errors are mapped but the
    /// backend should be dropped afterwards regardless.
    pub fn close(self) -> Result<(), AppError> {
        block_on(self.session.close()).map_err(|e| map_portal_error("close", e))
    }
}

fn key_state(press: bool) -> KeyState {
    if press {
        KeyState::Pressed
    } else {
        KeyState::Released
    }
}

/// Compile an xkb keymap from the default RMLVO names (the user's configured
/// layout via environment, falling back to the system default). Used to resolve
/// characters/keysyms to keycodes until the EIS-provided keymap is wired in.
fn default_keymap() -> Option<xkb::Keymap> {
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_names(&ctx, "", "", "", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
}
