use std::sync::Arc;
use std::time::{Duration, Instant};

use clarity_media::{
    ConnectionState, FrameSink, IceState, NativeHandle, NativeVideoSurface, Playback,
    PlaybackConfig, PlaybackError, PlaybackEvent, StreamStats,
};
use clarity_protocol::{
    ChatMessage, ClientMessage, ErrorCode, IceConfiguration, PROTOCOL_VERSION, RoomSnapshot,
    ServerMessage, SharingState, ViewerState,
};
use secrecy::ExposeSecret;
use tokio::sync::mpsc;

use crate::invite::Invitation;
use crate::presenter::{chat_envelope, queue_chat};
use crate::signaling::{
    SessionIdentity, SignalingClient, SignalingConfig, SignalingEvent, SignalingState,
    ice_refresh_delay, ice_refresh_retry, new_request_id,
};

/// How long a `Disconnected` connection may sit before the viewer asks the
/// presenter for an ICE restart; short blips recover on their own.
const ICE_RESTART_DEBOUNCE: Duration = Duration::from_secs(3);

/// Minimum spacing between automatic ICE restart requests, so a flapping
/// transport does not stack restarts. A manual request bypasses this.
const ICE_RESTART_MIN_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerPhase {
    Connecting,
    AwaitingApproval,
    Negotiating,
    Live,
}

/// What a host asks the running session to do.
#[derive(Debug)]
pub enum ViewerCommand {
    /// Send a chat message to the room.
    Chat(String),
    /// Playback volume, `0.0` (muted) to `1.0`.
    SetVolume(f64),
    /// Rename this viewer for the room and for chat.
    SetDisplayName(String),
    /// Ask the presenter for an ICE restart now.
    RestartIce,
    /// Leave the room; only this viewer is removed.
    Leave,
}

/// Progress reports for whatever front end hosts the session.
#[derive(Debug)]
pub enum ViewerUpdate {
    Signaling(SignalingState),
    Phase(ViewerPhase),
    SharingState(SharingState),
    /// Server-clock seconds until the room expires, from the latest snapshot.
    RoomExpiry {
        expires_in_seconds: u64,
    },
    PresenterConnected(bool),
    Connection(ConnectionState),
    Ice(IceState),
    Stats(StreamStats),
    /// A chat message from the room, unwrapped from the `ChatMessage`
    /// envelope.
    Chat {
        sender: String,
        text: String,
    },
    /// The native overlay surface video renders on, emitted once per playback
    /// when the requested native path came up. The host positions it with
    /// [`NativeVideoSurface::set_rect`]; without this update, video arrives
    /// through the frame sink instead.
    NativeSurface(Arc<NativeVideoSurface>),
}

/// How a session ended when it ended on the room's terms rather than an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    RoomClosed,
    RoomExpired,
    Rejected,
    Kicked,
    /// [`ViewerCommand::Leave`] ended the session.
    Left,
}

#[derive(Debug, thiserror::Error)]
pub enum ViewerError {
    #[error("the room did not accept this invitation: {0}")]
    AuthenticationFailed(String),
    #[error("media playback could not start: {0}")]
    PlaybackStart(#[from] PlaybackError),
    #[error("media playback failed: {0}")]
    Playback(String),
    #[error("signaling ended unexpectedly")]
    SignalingEnded,
}

pub struct ViewerSessionConfig {
    pub invitation: Invitation,
    pub display_name: Option<String>,
    /// Proves this viewer's identity when the room is friends-only; `None`
    /// fails authentication against such rooms.
    pub identity: Option<SessionIdentity>,
    pub force_relay: bool,
    /// Where decoded video is delivered. `Some` renders into the slot as RGBA
    /// (for an in-app display); `None` opens a native window.
    pub frames: Option<FrameSink>,
    /// Window handles for rendering video on a native Wayland subsurface
    /// instead of through `frames`. Keep `frames` set as well: if the overlay
    /// cannot be built, playback falls back to it silently.
    pub native: Option<NativeHandle>,
}

/// Joins a room as a viewer and renders the presenter's stream until the room
/// ends, the presenter removes the viewer, playback fails, or
/// [`ViewerCommand::Leave`] arrives.
///
/// The session mirrors the web viewer: admission may hold in
/// `AwaitingApproval`; every presenter offer (initial, source change, ICE
/// restart) renegotiates the same playback; signaling drops are ridden out
/// with resume while media keeps flowing, and a resume rejected after the
/// grace window falls back to a fresh join, awaiting a fresh offer.
pub struct ViewerSession {
    signaling: SignalingClient,
    signaling_events: mpsc::UnboundedReceiver<SignalingEvent>,
    updates: mpsc::UnboundedSender<ViewerUpdate>,
    commands: mpsc::UnboundedReceiver<ViewerCommand>,
    display_name: Option<String>,
    force_relay: bool,
    frames: Option<FrameSink>,
    native: Option<NativeHandle>,
    phase: ViewerPhase,
    self_peer_id: Option<String>,
    presenter_peer_id: Option<String>,
    /// The SDP origin session id of the last accepted offer; a non-restart
    /// offer with a different one comes from a rebuilt presenter connection.
    offer_session_id: Option<String>,
    ice: Option<IceConfiguration>,
    playback: Option<Playback>,
    playback_events: Option<mpsc::UnboundedReceiver<PlaybackEvent>>,
    /// Volume to apply to the current and any recreated playback.
    volume: Option<f64>,
    /// Chat sent before playback exists, flushed once it starts.
    pending_chat: Vec<String>,
    connection: ConnectionState,
    /// A debounced restart check is already scheduled.
    restart_check_pending: bool,
    restart_checks: mpsc::UnboundedSender<()>,
    restart_check_fired: mpsc::UnboundedReceiver<()>,
    last_restart_request: Option<Instant>,
    /// Fires shortly before the current ICE configuration's TURN credentials
    /// expire; an `ice:refresh` keeps restarts and rebuilds relay-capable.
    ice_refresh: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
}

impl ViewerSession {
    pub fn start(
        config: ViewerSessionConfig,
        updates: mpsc::UnboundedSender<ViewerUpdate>,
        commands: mpsc::UnboundedReceiver<ViewerCommand>,
    ) -> Self {
        let authentication = ClientMessage::AuthViewer {
            protocol_version: PROTOCOL_VERSION,
            request_id: new_request_id(),
            room_id: config.invitation.room_id.clone(),
            viewer_secret: config.invitation.secret.expose_secret().to_owned(),
            display_name: config.display_name.clone(),
        };
        let (restart_checks, restart_check_fired) = mpsc::unbounded_channel();
        let (signaling, signaling_events) = SignalingClient::connect(SignalingConfig {
            url: config.invitation.signaling_url.clone(),
            origin: config.invitation.origin.clone(),
            room_id: config.invitation.room_id.clone(),
            authentication,
            identity: config.identity,
        });
        Self {
            signaling,
            signaling_events,
            updates,
            commands,
            display_name: config.display_name,
            force_relay: config.force_relay,
            frames: config.frames,
            native: config.native,
            phase: ViewerPhase::Connecting,
            self_peer_id: None,
            presenter_peer_id: None,
            offer_session_id: None,
            ice: None,
            playback: None,
            playback_events: None,
            volume: None,
            pending_chat: Vec::new(),
            connection: ConnectionState::New,
            restart_check_pending: false,
            restart_checks,
            restart_check_fired,
            last_restart_request: None,
            ice_refresh: None,
        }
    }

    pub async fn run(mut self) -> Result<EndReason, ViewerError> {
        loop {
            let playback_event = async {
                match &mut self.playback_events {
                    Some(receiver) => receiver.recv().await,
                    None => std::future::pending().await,
                }
            };
            let ice_refresh_due = async {
                match &mut self.ice_refresh {
                    Some(sleep) => sleep.as_mut().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                command = self.commands.recv() => {
                    let Some(command) = command else {
                        return Ok(self.finish(EndReason::Left, true));
                    };
                    if let Some(end) = self.handle_command(command) {
                        return Ok(end);
                    }
                }
                () = ice_refresh_due => {
                    self.signaling.send(ClientMessage::IceRefresh {
                        protocol_version: PROTOCOL_VERSION,
                        request_id: new_request_id(),
                    });
                    // Retry cadence until the refreshed configuration arrives
                    // and re-arms the timer from its expiry.
                    self.ice_refresh = Some(Box::pin(tokio::time::sleep(ice_refresh_retry())));
                }
                _ = self.restart_check_fired.recv() => {
                    // The debounce elapsed; ask for a restart only if the
                    // connection never came back on its own.
                    self.restart_check_pending = false;
                    if matches!(
                        self.connection,
                        ConnectionState::Disconnected | ConnectionState::Failed
                    ) {
                        self.request_ice_restart(false);
                    }
                }
                event = playback_event => {
                    if let Some(event) = event
                        && let Some(outcome) = self.handle_playback(event)?
                    {
                        return Ok(outcome);
                    }
                }
                event = self.signaling_events.recv() => {
                    let Some(event) = event else {
                        return Err(ViewerError::SignalingEnded);
                    };
                    match event {
                        SignalingEvent::State(state) => {
                            let _ = self.updates.send(ViewerUpdate::Signaling(state));
                        }
                        SignalingEvent::Message(message) => {
                            if let Some(outcome) = self.handle_message(*message)? {
                                return Ok(outcome);
                            }
                        }
                    }
                }
            }
        }
    }

    fn handle_command(&mut self, command: ViewerCommand) -> Option<EndReason> {
        match command {
            ViewerCommand::Chat(text) => {
                let sender = self
                    .display_name
                    .clone()
                    .unwrap_or_else(|| "Viewer".to_owned());
                if let Some(envelope) = chat_envelope(&sender, &text) {
                    match &self.playback {
                        Some(playback) => playback.send_chat(&envelope),
                        None => queue_chat(&mut self.pending_chat, envelope),
                    }
                } else {
                    tracing::warn!("dropping an oversized outbound chat message");
                }
            }
            ViewerCommand::SetVolume(level) => {
                self.volume = Some(level);
                if let Some(playback) = &self.playback {
                    playback.set_volume(level);
                }
            }
            ViewerCommand::SetDisplayName(name) => {
                self.display_name = Some(name.clone());
                self.signaling.send(ClientMessage::ViewerUpdateDisplayName {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: new_request_id(),
                    display_name: Some(name),
                });
            }
            ViewerCommand::RestartIce => self.request_ice_restart(true),
            ViewerCommand::Leave => return Some(self.finish(EndReason::Left, true)),
        }
        None
    }

    fn handle_message(&mut self, message: ServerMessage) -> Result<Option<EndReason>, ViewerError> {
        match message {
            ServerMessage::AuthSucceeded {
                peer_id,
                snapshot,
                ice_configuration,
                ..
            } => {
                self.self_peer_id = Some(peer_id.clone());
                self.ice_refresh = Some(Box::pin(tokio::time::sleep(ice_refresh_delay(
                    &ice_configuration.expires_at,
                ))));
                self.ice = Some(ice_configuration);
                let approved = find_self(&snapshot, &peer_id)
                    .is_some_and(|viewer| viewer.viewer_state == Some(ViewerState::Approved));
                self.set_phase(if approved {
                    ViewerPhase::Negotiating
                } else {
                    ViewerPhase::AwaitingApproval
                });
                self.report_snapshot(&snapshot);
            }
            ServerMessage::RoomSnapshot { snapshot, .. } => self.report_snapshot(&snapshot),
            ServerMessage::RoomSharingStateUpdated { sharing_state, .. } => {
                let _ = self.updates.send(ViewerUpdate::SharingState(sharing_state));
            }
            ServerMessage::ViewerApproved { peer_id, .. } => {
                if Some(&peer_id) == self.self_peer_id.as_ref() {
                    self.set_phase(ViewerPhase::Negotiating);
                }
            }
            ServerMessage::ViewerRejected { peer_id, .. } => {
                if Some(&peer_id) == self.self_peer_id.as_ref() {
                    return Ok(Some(self.finish(EndReason::Rejected, false)));
                }
            }
            ServerMessage::ViewerKicked { peer_id, .. } => {
                if Some(&peer_id) == self.self_peer_id.as_ref() {
                    return Ok(Some(self.finish(EndReason::Kicked, false)));
                }
            }
            ServerMessage::SignalOffer {
                source_peer_id,
                sdp,
                ice_restart,
                ..
            } => {
                self.presenter_peer_id = Some(source_peer_id);
                self.set_phase(ViewerPhase::Negotiating);
                // A re-offer either renegotiates the presenter's existing
                // connection (an ICE restart, or an in-place track change such
                // as the web presenter adding audio at idle -> live) or comes
                // from a rebuilt one after an unrecoverable failure. Only the
                // rebuild may recreate playback: answering an in-place
                // renegotiation from a fresh transport would hand the
                // presenter's live connection an unusable answer. The SDP
                // origin's session id tells them apart; it is stable across
                // renegotiations of one connection and fresh for a new one.
                let session_id = sdp_session_id(&sdp).map(str::to_owned);
                let same_connection =
                    ice_restart || (session_id.is_some() && session_id == self.offer_session_id);
                if !same_connection && self.playback.is_some() {
                    self.finish_playback();
                }
                if session_id.is_some() {
                    self.offer_session_id = session_id;
                }
                self.ensure_playback()?.accept_offer(&sdp)?;
            }
            ServerMessage::SignalIceCandidate {
                candidate,
                sdp_m_line_index,
                ..
            } => {
                if let Some(playback) = &self.playback {
                    playback
                        .add_remote_candidate(u32::from(sdp_m_line_index.unwrap_or(0)), &candidate);
                }
            }
            ServerMessage::IceConfiguration { configuration, .. } => {
                self.ice_refresh = Some(Box::pin(tokio::time::sleep(ice_refresh_delay(
                    &configuration.expires_at,
                ))));
                self.ice = Some(configuration);
            }
            ServerMessage::PresenterDisconnected { .. } => {
                let _ = self.updates.send(ViewerUpdate::PresenterConnected(false));
            }
            ServerMessage::PresenterResumed { .. } => {
                let _ = self.updates.send(ViewerUpdate::PresenterConnected(true));
            }
            ServerMessage::RoomClosed { .. } => {
                return Ok(Some(self.finish(EndReason::RoomClosed, false)));
            }
            ServerMessage::RoomExpired { .. } => {
                return Ok(Some(self.finish(EndReason::RoomExpired, false)));
            }
            ServerMessage::AuthFailed { message, .. } => {
                self.finish_playback();
                return Err(ViewerError::AuthenticationFailed(message));
            }
            ServerMessage::Error { code, message, .. } => match code {
                ErrorCode::RoomClosed => {
                    return Ok(Some(self.finish(EndReason::RoomClosed, false)));
                }
                ErrorCode::RoomExpired => {
                    return Ok(Some(self.finish(EndReason::RoomExpired, false)));
                }
                // A rejected message (a stale destination, a rate limit) is
                // not the end of the session.
                _ => tracing::warn!(?code, %message, "the server rejected a message"),
            },
            _ => {}
        }
        Ok(None)
    }

    fn handle_playback(&mut self, event: PlaybackEvent) -> Result<Option<EndReason>, ViewerError> {
        match event {
            PlaybackEvent::Answer { sdp } => {
                if let Some(destination) = self.presenter_peer_id.clone() {
                    self.signaling.send(ClientMessage::SignalAnswer {
                        protocol_version: PROTOCOL_VERSION,
                        request_id: new_request_id(),
                        destination_peer_id: destination,
                        sdp,
                    });
                }
            }
            PlaybackEvent::IceCandidate {
                candidate,
                sdp_m_line_index,
            } => {
                if let Some(destination) = self.presenter_peer_id.clone() {
                    self.signaling.send(ClientMessage::SignalIceCandidate {
                        protocol_version: PROTOCOL_VERSION,
                        request_id: new_request_id(),
                        destination_peer_id: destination,
                        candidate,
                        sdp_mid: None,
                        sdp_m_line_index: Some(u16::try_from(sdp_m_line_index).unwrap_or(u16::MAX)),
                    });
                }
            }
            PlaybackEvent::ConnectionState(state) => {
                self.connection = state;
                match state {
                    ConnectionState::Connected => self.set_phase(ViewerPhase::Live),
                    // Ask the presenter to restart if this side sees the
                    // connection fail — the presenter may not observe the same
                    // failure, so the request covers an asymmetric break.
                    ConnectionState::Failed => self.request_ice_restart(false),
                    // A disconnect often heals on its own; give it the
                    // debounce window before asking for a restart.
                    ConnectionState::Disconnected => self.schedule_restart_check(),
                    _ => {}
                }
                let _ = self.updates.send(ViewerUpdate::Connection(state));
            }
            PlaybackEvent::IceState(state) => {
                let _ = self.updates.send(ViewerUpdate::Ice(state));
            }
            PlaybackEvent::Stats(stats) => {
                let _ = self.updates.send(ViewerUpdate::Stats(stats));
            }
            PlaybackEvent::Chat { text } => match ChatMessage::from_json(&text) {
                Some(chat) => {
                    let _ = self.updates.send(ViewerUpdate::Chat {
                        sender: chat.sender,
                        text: chat.text,
                    });
                }
                None => {
                    tracing::warn!("dropping a chat payload that is not a ChatMessage envelope");
                }
            },
            PlaybackEvent::Ended { reason } => {
                self.finish_playback();
                self.signaling.disconnect(true);
                return Err(ViewerError::Playback(reason));
            }
        }
        Ok(None)
    }

    /// Schedules a debounced connectivity check; when it fires, a connection
    /// still down asks the presenter for an ICE restart.
    fn schedule_restart_check(&mut self) {
        if self.restart_check_pending {
            return;
        }
        self.restart_check_pending = true;
        let checks = self.restart_checks.clone();
        tokio::spawn(async move {
            tokio::time::sleep(ICE_RESTART_DEBOUNCE).await;
            let _ = checks.send(());
        });
    }

    fn request_ice_restart(&mut self, manual: bool) {
        if !manual
            && self
                .last_restart_request
                .is_some_and(|at| at.elapsed() < ICE_RESTART_MIN_INTERVAL)
        {
            return;
        }
        let Some(destination) = self.presenter_peer_id.clone() else {
            return;
        };
        self.last_restart_request = Some(Instant::now());
        self.signaling.send(ClientMessage::SignalIceRestart {
            protocol_version: PROTOCOL_VERSION,
            request_id: new_request_id(),
            destination_peer_id: destination,
        });
    }

    fn ensure_playback(&mut self) -> Result<&Playback, ViewerError> {
        if self.playback.is_none() {
            let ice = self.ice.clone().ok_or_else(|| {
                ViewerError::Playback("an offer arrived before the ICE configuration".into())
            })?;
            let (playback, events) = Playback::start(PlaybackConfig {
                ice,
                force_relay: self.force_relay,
                frames: self.frames.clone(),
                native: self.native,
                audio_samples: None,
            })?;
            if let Some(surface) = playback.native_surface() {
                let _ = self.updates.send(ViewerUpdate::NativeSurface(surface));
            }
            if let Some(level) = self.volume {
                playback.set_volume(level);
            }
            for envelope in self.pending_chat.drain(..) {
                playback.send_chat(&envelope);
            }
            self.playback = Some(playback);
            self.playback_events = Some(events);
        }
        Ok(self.playback.as_ref().expect("playback just ensured"))
    }

    fn report_snapshot(&mut self, snapshot: &RoomSnapshot) {
        let _ = self
            .updates
            .send(ViewerUpdate::SharingState(snapshot.sharing_state));
        let _ = self.updates.send(ViewerUpdate::RoomExpiry {
            expires_in_seconds: snapshot.expires_in_seconds,
        });
        let _ = self.updates.send(ViewerUpdate::PresenterConnected(
            snapshot.presenter_connected,
        ));
    }

    fn set_phase(&mut self, phase: ViewerPhase) {
        if self.phase != phase {
            self.phase = phase;
            let _ = self.updates.send(ViewerUpdate::Phase(phase));
        }
    }

    fn finish(&mut self, reason: EndReason, announce_leave: bool) -> EndReason {
        self.finish_playback();
        self.signaling.disconnect(announce_leave);
        reason
    }

    fn finish_playback(&mut self) {
        self.playback_events = None;
        if let Some(playback) = self.playback.take() {
            playback.close();
        }
        self.connection = ConnectionState::New;
    }
}

/// The `<sess-id>` field of the SDP origin (`o=`) line, or `None` when the
/// SDP has no parseable origin.
fn sdp_session_id(sdp: &str) -> Option<&str> {
    sdp.lines()
        .find_map(|line| line.strip_prefix("o="))
        .and_then(|origin| origin.split_whitespace().nth(1))
}

fn find_self<'a>(
    snapshot: &'a RoomSnapshot,
    peer_id: &str,
) -> Option<&'a clarity_protocol::PeerSnapshot> {
    snapshot
        .pending_viewers
        .iter()
        .chain(snapshot.approved_viewers.iter())
        .find(|viewer| viewer.peer_id == peer_id)
}
