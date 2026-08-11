use std::os::fd::OwnedFd;

use ashpd::desktop::PersistMode;
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use tokio::sync::oneshot;

#[derive(Debug, Clone, Default)]
pub struct CaptureRequest {
    pub show_cursor: bool,
    /// Token from a previous capture; presenting it lets the system skip the
    /// picker and reuse the same screen or window.
    pub restore_token: Option<String>,
    /// Ask the system to remember this choice and issue a token for next
    /// time. When off, no grant is retained and
    /// [`CaptureStream::restore_token`] stays empty.
    pub remember: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error(
        "screen capture is not available; a desktop portal with ScreenCast support is required"
    )]
    Unavailable,
    #[error("the screen selection was cancelled")]
    Cancelled,
    #[error("screen capture could not start: {0}")]
    Failed(String),
}

/// A negotiated screen or window capture: the user has picked a source
/// through the system dialog and the compositor is streaming it. The capture
/// stays live while this value exists and is revoked when it is dropped, so
/// it must outlive the broadcast that consumes it.
#[derive(Debug)]
pub struct CaptureStream {
    pub(crate) fd: OwnedFd,
    pub(crate) node_id: u32,
    restore_token: Option<String>,
    /// Dropping this releases the portal session that keeps the compositor
    /// streaming.
    _session_guard: oneshot::Sender<()>,
}

impl CaptureStream {
    /// Opens the system picker and waits for the user's choice. This can take
    /// arbitrarily long — the user is choosing — and resolves to `Cancelled`
    /// if they dismiss the dialog.
    pub async fn open(request: CaptureRequest) -> Result<Self, CaptureError> {
        let proxy = Screencast::new()
            .await
            .map_err(|_| CaptureError::Unavailable)?;
        let session = proxy
            .create_session(Default::default())
            .await
            .map_err(|error| CaptureError::Failed(error.to_string()))?;
        let cursor = if request.show_cursor {
            CursorMode::Embedded
        } else {
            CursorMode::Hidden
        };
        let persist_mode = if request.remember {
            PersistMode::ExplicitlyRevoked
        } else {
            PersistMode::DoNot
        };
        proxy
            .select_sources(
                &session,
                SelectSourcesOptions::default()
                    .set_cursor_mode(cursor)
                    .set_sources(SourceType::Monitor | SourceType::Window)
                    .set_multiple(false)
                    .set_restore_token(request.restore_token.as_deref())
                    .set_persist_mode(persist_mode),
            )
            .await
            .map_err(|error| CaptureError::Failed(error.to_string()))?;
        let response = proxy
            .start(&session, None, Default::default())
            .await
            .map_err(|error| CaptureError::Failed(error.to_string()))?
            .response()
            .map_err(|error| match error {
                ashpd::Error::Response(_) => CaptureError::Cancelled,
                other => CaptureError::Failed(other.to_string()),
            })?;
        let node_id = response
            .streams()
            .first()
            .map(|stream| stream.pipe_wire_node_id())
            .ok_or_else(|| CaptureError::Failed("the compositor provided no stream".into()))?;
        let restore_token = response.restore_token().map(str::to_owned);
        let fd = proxy
            .open_pipe_wire_remote(&session, Default::default())
            .await
            .map_err(|error| CaptureError::Failed(error.to_string()))?;

        // The compositor stops streaming when the portal session closes, so
        // the session must live exactly as long as this capture.
        let (guard, released) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = released.await;
            let _ = session.close().await;
        });

        Ok(Self {
            fd,
            node_id,
            restore_token,
            _session_guard: guard,
        })
    }

    /// Token to present next time to reuse the same source without a picker.
    pub fn restore_token(&self) -> Option<&str> {
        self.restore_token.as_deref()
    }
}
