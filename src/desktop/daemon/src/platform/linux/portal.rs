//! XDG portal session manager: ScreenCast (DesktopCtl-0f2).
//!
//! Manages an `org.freedesktop.portal.ScreenCast` session for the daemon
//! lifetime. The daemon is synchronous, so the async `ashpd` API is driven
//! with `futures_lite::future::block_on`.
//!
//! Flow (see `tmp/ubuntu-spec.md` "Capture"):
//!   1. `CreateSession`
//!   2. `SelectSources` (one monitor, `persist_mode = ExplicitlyRevoked` (=2),
//!      cursor embedded)
//!   3. `Start`
//!   4. `OpenPipeWireRemote` -> PipeWire fd + stream node id(s)
//!
//! The session is held open for the daemon lifetime. Restore tokens returned
//! by the portal are single-use and rotate on each restoration; we store the
//! latest one so it can be persisted (disk persistence is a TODO).

use std::os::fd::OwnedFd;

use ashpd::Error as AshpdError;
use ashpd::desktop::{
    PersistMode, Session,
    screencast::{CursorMode, Screencast, SourceType},
};
use desktop_core::error::AppError;
use futures_lite::future::block_on;
use serde_json::json;

/// `persist_mode = 2` (`XDP_PERSIST_MODE_PERSISTENT`): persist until the user
/// explicitly revokes the grant. Best-effort promptless restart per spec.
const PERSIST_MODE: PersistMode = PersistMode::ExplicitlyRevoked;

/// A single PipeWire video node selected for capture.
#[derive(Debug, Clone, Copy)]
pub struct StreamNode {
    /// PipeWire node id to connect a stream to.
    pub node_id: u32,
    /// Size in the compositor coordinate space, if reported by the portal.
    /// Note: NOT necessarily pixels (see spec "Coordinate Model").
    pub size: Option<(i32, i32)>,
    /// Position in the compositor coordinate space, if reported.
    pub position: Option<(i32, i32)>,
}

/// An open ScreenCast portal session plus the PipeWire remote fd and the
/// node ids returned by the portal. Held for the daemon lifetime.
///
/// The proxy + session use a `'static` lifetime: `ashpd` keeps the session
/// D-Bus connection in a process-global `OnceLock`, and the proxy is created
/// from a `'static` interface-name string literal.
pub struct ScreenCastSession {
    // Kept alive for the daemon lifetime; the portal grant is bound to the
    // session object remaining open. `_proxy` is retained for symmetry / future
    // calls (e.g. re-`SelectSources` on restoration).
    _proxy: Screencast<'static>,
    session: Session<'static, Screencast<'static>>,
    pipewire_fd: OwnedFd,
    streams: Vec<StreamNode>,
    /// Latest restore token, if the portal returned one. Rotates per spec.
    restore_token: Option<String>,
}

impl ScreenCastSession {
    /// The raw PipeWire remote file descriptor. Borrowed; callers that need an
    /// owned fd should use [`Self::pipewire_fd_cloned`].
    pub fn pipewire_raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.pipewire_fd.as_raw_fd()
    }

    /// A cloned owned copy of the PipeWire remote fd, suitable for handing to a
    /// PipeWire context (`Context::connect_fd` consumes an `OwnedFd`).
    pub fn pipewire_fd_cloned(&self) -> Result<OwnedFd, AppError> {
        self.pipewire_fd
            .try_clone()
            .map_err(|e| capture_closed(format!("failed to clone PipeWire fd: {e}")))
    }

    /// All PipeWire stream nodes selected for this session.
    pub fn stream_nodes(&self) -> &[StreamNode] {
        &self.streams
    }

    /// The primary (first) PipeWire node id, i.e. the default monitor.
    pub fn primary_node_id(&self) -> Result<u32, AppError> {
        self.streams
            .first()
            .map(|s| s.node_id)
            .ok_or_else(|| capture_closed("no PipeWire streams in ScreenCast session"))
    }

    /// The latest restore token, if any.
    pub fn restore_token(&self) -> Option<&str> {
        self.restore_token.as_deref()
    }

    /// Persist a newly returned restore token (rotates on each restoration).
    ///
    /// TODO: write the token to disk so a daemon restart can attempt promptless
    /// restoration. For now it is held in memory only.
    pub fn set_restore_token(&mut self, token: impl Into<String>) {
        self.restore_token = Some(token.into());
    }

    /// Close the underlying portal session. Best-effort.
    pub fn close(&self) {
        let _ = block_on(self.session.close());
    }
}

/// Open a ScreenCast session: one monitor, cursor embedded, persistent grant.
///
/// Synchronous wrapper around the async portal flow.
pub fn start_screencast() -> Result<ScreenCastSession, AppError> {
    block_on(start_screencast_async())
}

async fn start_screencast_async() -> Result<ScreenCastSession, AppError> {
    let proxy = Screencast::new().await.map_err(map_ashpd_err)?;

    let session = proxy.create_session().await.map_err(map_ashpd_err)?;

    // Default source: a single monitor, with the cursor embedded into the
    // stream buffers, persisting until explicitly revoked.
    proxy
        .select_sources(
            &session,
            CursorMode::Embedded,
            SourceType::Monitor.into(),
            /* multiple = */ false,
            /* restore_token = */ None,
            PERSIST_MODE,
        )
        .await
        .map_err(map_ashpd_err)?;

    let response = proxy
        .start(&session, None)
        .await
        .map_err(map_ashpd_err)?
        .response()
        .map_err(map_ashpd_err)?;

    let restore_token = response.restore_token().map(ToOwned::to_owned);

    let streams: Vec<StreamNode> = response
        .streams()
        .iter()
        .map(|s| StreamNode {
            node_id: s.pipe_wire_node_id(),
            size: s.size(),
            position: s.position(),
        })
        .collect();

    if streams.is_empty() {
        return Err(capture_closed(
            "ScreenCast portal returned no streams (selection cancelled?)",
        ));
    }

    let pipewire_fd = proxy
        .open_pipe_wire_remote(&session)
        .await
        .map_err(map_ashpd_err)?;

    Ok(ScreenCastSession {
        _proxy: proxy,
        session,
        pipewire_fd,
        streams,
        restore_token,
    })
}

/// Map an `ashpd` error to an [`AppError`] tagged with a spec failure state.
fn map_ashpd_err(err: AshpdError) -> AppError {
    use ashpd::PortalError;
    use ashpd::desktop::ResponseError;

    match err {
        // The user cancelled or otherwise denied the portal dialog.
        AshpdError::Response(ResponseError::Cancelled) => {
            AppError::permission_denied("screen capture permission was denied")
                .with_details(json!({ "failure_state": "permission_denied" }))
        }
        AshpdError::Response(ResponseError::Other) => {
            AppError::permission_denied("screen capture request failed")
                .with_details(json!({ "failure_state": "permission_required" }))
        }
        // No portal frontend / interface available.
        AshpdError::PortalNotFound(_) => {
            AppError::backend_unavailable("XDG ScreenCast portal not available")
                .with_details(json!({ "failure_state": "portal_unavailable" }))
        }
        AshpdError::RequiresVersion(_, _) => {
            AppError::backend_unavailable("XDG ScreenCast portal version too old")
                .with_details(json!({ "failure_state": "portal_unavailable" }))
        }
        AshpdError::Portal(PortalError::Failed(msg)) => {
            AppError::backend_unavailable(format!("ScreenCast portal failed: {msg}"))
                .with_details(json!({ "failure_state": "capture_session_closed" }))
        }
        AshpdError::Portal(_) => AppError::backend_unavailable("ScreenCast portal error")
            .with_details(json!({ "failure_state": "portal_unavailable" })),
        other => AppError::backend_unavailable(format!("ScreenCast portal error: {other}"))
            .with_details(json!({ "failure_state": "portal_unavailable" })),
    }
}

fn capture_closed(msg: impl Into<String>) -> AppError {
    AppError::backend_unavailable(msg)
        .with_details(json!({ "failure_state": "capture_session_closed" }))
}
