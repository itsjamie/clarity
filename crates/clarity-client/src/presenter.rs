use std::collections::HashSet;
use std::pin::Pin;
use std::time::Duration;

use clarity_media::{
    AudioCapture, Broadcast, BroadcastConfig, BroadcastError, BroadcastEvent, ConnectionState,
    EncoderSettings, FrameSink, SenderStats, SourceConfig, VideoCodecId,
};
use clarity_protocol::{
    ChatMessage, ClientMessage, ErrorCode, PROTOCOL_VERSION, PeerSnapshot, RoomSnapshot,
    ServerMessage, SharingState,
};
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::mpsc;

use crate::signaling::{
    SignalingClient, SignalingConfig, SignalingEvent, SignalingState, ice_refresh_delay,
    ice_refresh_retry, new_request_id,
};

/// How long an ICE restart is given to recover a viewer's connection before it
/// is rebuilt from scratch. Matches the web presenter's grace window.
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(8);

/// How long `CloseRoom` waits for the server's `room:closed` confirmation
/// before finishing anyway. Covers a close issued while the connection is
/// down: the queued `room:close` gets one reconnect cycle to be delivered.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct PresenterSessionConfig {
    pub room_id: String,
    pub presenter_secret: SecretString,
    pub signaling_url: String,
    pub origin: String,
    /// The capture to open the room with. `None` opens the room idle — the
    /// broadcast runs on the internal placeholder until
    /// [`PresenterCommand::StartShare`].
    pub source: Option<SourceConfig>,
    /// What sound accompanies the picture; an uncapturable audio source
    /// downgrades to video-only with a log line rather than failing.
    pub audio: AudioCapture,
    /// Ranked codec preference for the offer, best first; empty means the
    /// media engine's default order. Codecs without an installed encoder are
    /// skipped.
    pub video_codecs: Vec<VideoCodecId>,
    /// The maximum capture/encode frame rate in fps (the profile's 30 or 60).
    pub frame_rate: u32,
    /// The largest frame (width, height) fed to the encoders; a bigger
    /// capture is scaled down preserving aspect. `None` keeps the 2560x1440
    /// default.
    pub capture_ceiling: Option<(u32, u32)>,
    pub bitrate_kbps: u32,
    /// Adapt each viewer's rate to transport feedback; off holds the
    /// configured bitrate regardless of network conditions.
    pub adaptive: bool,
    pub force_relay: bool,
    /// A local self-preview of the captured screen, as RGBA frames. `Some` lets
    /// the presenter see what they are sharing; `None` skips the tap.
    pub preview_frames: Option<FrameSink>,
    /// The name outgoing chat carries as its sender; `None` reads as
    /// "Presenter".
    pub display_name: Option<String>,
    /// Approve join requests the moment they arrive. Off, every request
    /// surfaces as [`PresenterUpdate::JoinRequested`] and waits for
    /// [`PresenterCommand::ApproveViewer`] or
    /// [`PresenterCommand::RejectViewer`].
    pub auto_approve: bool,
}

/// What a host asks the running session to do. All room and sharing
/// transitions flow through here; the session owns the ordering.
#[derive(Debug)]
pub enum PresenterCommand {
    /// Send a chat message to the room as the presenter.
    Chat(String),
    /// Begin sharing this capture. Connections are untouched; the capture
    /// head is swapped in and the room goes `Live`.
    StartShare(SourceConfig),
    /// Swap the shared capture mid-stream without renegotiating viewers.
    SwitchSource(SourceConfig),
    PauseShare,
    ResumeShare,
    /// Return the room to idle: the capture ends and its grant is released,
    /// but the room, its connections, and chat stay up.
    StopShare,
    /// End the room for everyone.
    CloseRoom,
    /// Disconnect without closing the room; the server keeps the presenter
    /// resumable within its grace window.
    Leave,
    ApproveViewer(String),
    RejectViewer(String),
    KickViewer(String),
    /// Restart ICE on one viewer's connection now.
    RestartIce(String),
    /// Rename the presenter as chat sees it.
    SetDisplayName(String),
}

/// Progress reports for whatever front end hosts the session.
#[derive(Debug)]
pub enum PresenterUpdate {
    Signaling(SignalingState),
    /// The room's sharing state as this session drives it.
    SharingState(SharingState),
    /// Server-clock seconds until the room expires, from the latest snapshot.
    RoomExpiry {
        expires_in_seconds: u64,
    },
    /// A viewer asked to join. With `auto_approve` off, the request waits for
    /// [`PresenterCommand::ApproveViewer`] or
    /// [`PresenterCommand::RejectViewer`].
    JoinRequested {
        peer_id: String,
        display_name: Option<String>,
        friend_code: Option<String>,
    },
    /// An approved viewer's connection was created.
    ViewerJoined {
        peer_id: String,
        display_name: Option<String>,
    },
    ViewerLeft {
        peer_id: String,
    },
    ViewerConnection {
        peer_id: String,
        state: ConnectionState,
    },
    ViewerStats {
        peer_id: String,
        stats: SenderStats,
    },
    /// A chat message from a viewer. The broadcast has already relayed it to
    /// the other viewers.
    Chat {
        peer_id: String,
        sender: String,
        text: String,
    },
    /// A share command failed; the broadcast stays in its previous state.
    ShareFailed {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenterEnd {
    /// The room was closed — by [`PresenterCommand::CloseRoom`] or by the
    /// server on the presenter's behalf.
    RoomClosed,
    RoomExpired,
    /// [`PresenterCommand::Leave`] disconnected the session; the room stays
    /// open and resumable.
    Left,
}

#[derive(Debug, thiserror::Error)]
pub enum PresenterError {
    #[error("the room did not accept the presenter secret: {0}")]
    AuthenticationFailed(String),
    #[error("the broadcast could not start: {0}")]
    BroadcastStart(#[from] BroadcastError),
    #[error("the broadcast failed: {0}")]
    Broadcast(String),
    #[error("signaling ended unexpectedly")]
    SignalingEnded,
}

/// Hosts a room as its presenter: authenticates, admits viewers, and streams
/// one broadcast to every approved viewer until the room ends.
///
/// The room's life is independent of sharing. The broadcast starts on the
/// internal idle placeholder when no source is configured, and
/// [`PresenterCommand::StopShare`] returns to it without touching viewer
/// connections, so chat keeps flowing between shares. Only
/// [`PresenterCommand::CloseRoom`] ends the room for everyone.
///
/// Admission mirrors the web presenter: the set of active viewer connections
/// always follows the server's room snapshot, so approvals, departures, kicks,
/// and capacity changes converge from one authority.
pub struct PresenterSession {
    signaling: SignalingClient,
    signaling_events: mpsc::UnboundedReceiver<SignalingEvent>,
    updates: mpsc::UnboundedSender<PresenterUpdate>,
    commands: mpsc::UnboundedReceiver<PresenterCommand>,
    /// Consumed when the broadcast starts on first authentication.
    source: Option<SourceConfig>,
    /// The presenter's self-preview sink, handed to the broadcast at start.
    preview_frames: Option<FrameSink>,
    audio: AudioCapture,
    video_codecs: Vec<VideoCodecId>,
    frame_rate: u32,
    capture_ceiling: Option<(u32, u32)>,
    bitrate_kbps: u32,
    adaptive: bool,
    force_relay: bool,
    display_name: String,
    auto_approve: bool,
    sharing: SharingState,
    broadcast: Option<Broadcast>,
    broadcast_events: Option<mpsc::UnboundedReceiver<BroadcastEvent>>,
    self_peer_id: Option<String>,
    /// The room actor registers the session before the auth reply is written,
    /// so a `room:snapshot` can arrive on the wire ahead of `auth:succeeded`
    /// while reflecting newer room state. It is held here and re-applied once
    /// the broadcast exists, or its viewers would never get connections.
    pending_snapshot: Option<RoomSnapshot>,
    /// Chat sent before the broadcast exists, flushed once it starts.
    pending_chat: Vec<String>,
    active_viewers: HashSet<String>,
    approvals_sent: HashSet<String>,
    /// Pending viewers already surfaced as `JoinRequested`, so a snapshot on
    /// resume does not repeat requests the host has seen.
    announced_requests: HashSet<String>,
    /// Viewers with an ICE restart in flight. A viewer leaves the set when it
    /// reconnects; one still present when its recovery deadline fires is
    /// rebuilt. Guards against stacking restarts on repeated failure events.
    recovering: HashSet<String>,
    recovery_deadlines: mpsc::UnboundedSender<String>,
    recovery_fired: mpsc::UnboundedReceiver<String>,
    /// Armed by `CloseRoom`; firing before `room:closed` arrives finishes the
    /// session anyway.
    close_deadline: Option<Pin<Box<tokio::time::Sleep>>>,
    /// Fires shortly before the current ICE configuration's TURN credentials
    /// expire; an `ice:refresh` keeps restarts and rebuilds relay-capable.
    ice_refresh: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl PresenterSession {
    pub fn start(
        config: PresenterSessionConfig,
        updates: mpsc::UnboundedSender<PresenterUpdate>,
        commands: mpsc::UnboundedReceiver<PresenterCommand>,
    ) -> Self {
        let authentication = ClientMessage::AuthPresenter {
            protocol_version: PROTOCOL_VERSION,
            request_id: new_request_id(),
            room_id: config.room_id.clone(),
            presenter_secret: config.presenter_secret.expose_secret().to_owned(),
        };
        let (recovery_deadlines, recovery_fired) = mpsc::unbounded_channel();
        let (signaling, signaling_events) = SignalingClient::connect(SignalingConfig {
            url: config.signaling_url,
            origin: config.origin,
            room_id: config.room_id,
            authentication,
            identity: None,
        });
        let sharing = if config.source.is_some() {
            SharingState::Live
        } else {
            SharingState::Idle
        };
        Self {
            signaling,
            signaling_events,
            updates,
            commands,
            source: config.source,
            preview_frames: config.preview_frames,
            audio: config.audio,
            video_codecs: config.video_codecs,
            frame_rate: config.frame_rate,
            capture_ceiling: config.capture_ceiling,
            bitrate_kbps: config.bitrate_kbps,
            adaptive: config.adaptive,
            force_relay: config.force_relay,
            display_name: config
                .display_name
                .unwrap_or_else(|| "Presenter".to_owned()),
            auto_approve: config.auto_approve,
            sharing,
            broadcast: None,
            broadcast_events: None,
            self_peer_id: None,
            pending_snapshot: None,
            pending_chat: Vec::new(),
            active_viewers: HashSet::new(),
            approvals_sent: HashSet::new(),
            announced_requests: HashSet::new(),
            recovering: HashSet::new(),
            recovery_deadlines,
            recovery_fired,
            close_deadline: None,
            ice_refresh: None,
        }
    }

    pub async fn run(mut self) -> Result<PresenterEnd, PresenterError> {
        loop {
            let broadcast_event = async {
                match &mut self.broadcast_events {
                    Some(receiver) => receiver.recv().await,
                    None => std::future::pending().await,
                }
            };
            let close_timeout = async {
                match &mut self.close_deadline {
                    Some(sleep) => sleep.as_mut().await,
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
                    // A dropped command handle leaves the room open; closing
                    // is always an explicit CloseRoom.
                    let Some(command) = command else {
                        return Ok(self.finish(PresenterEnd::Left));
                    };
                    if let Some(end) = self.handle_command(command) {
                        return Ok(end);
                    }
                }
                () = close_timeout => {
                    tracing::warn!("the room close was not confirmed in time; finishing anyway");
                    return Ok(self.finish(PresenterEnd::RoomClosed));
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
                peer = self.recovery_fired.recv() => {
                    if let Some(peer) = peer {
                        self.rebuild_if_unrecovered(peer);
                    }
                }
                event = broadcast_event => {
                    if let Some(event) = event {
                        self.handle_broadcast(event)?;
                    }
                }
                event = self.signaling_events.recv() => {
                    let Some(event) = event else {
                        return Err(PresenterError::SignalingEnded);
                    };
                    match event {
                        SignalingEvent::State(state) => {
                            let _ = self.updates.send(PresenterUpdate::Signaling(state));
                        }
                        SignalingEvent::Message(message) => {
                            if let Some(end) = self.handle_message(*message)? {
                                return Ok(end);
                            }
                        }
                    }
                }
            }
        }
    }

    fn handle_command(&mut self, command: PresenterCommand) -> Option<PresenterEnd> {
        match command {
            PresenterCommand::Chat(text) => {
                let envelope = chat_envelope(&self.display_name, &text);
                match &self.broadcast {
                    Some(broadcast) => broadcast.send_chat(&envelope),
                    None => self.pending_chat.push(envelope),
                }
            }
            PresenterCommand::StartShare(source) | PresenterCommand::SwitchSource(source) => {
                self.share(source);
            }
            PresenterCommand::PauseShare => {
                if self.sharing == SharingState::Live
                    && let Some(broadcast) = &self.broadcast
                {
                    broadcast.pause();
                    self.set_sharing(SharingState::Paused);
                }
            }
            PresenterCommand::ResumeShare => {
                if self.sharing == SharingState::Paused
                    && let Some(broadcast) = &self.broadcast
                {
                    broadcast.resume();
                    self.set_sharing(SharingState::Live);
                }
            }
            PresenterCommand::StopShare => match &self.broadcast {
                Some(broadcast) => match broadcast.idle() {
                    Ok(()) => {
                        // A share stopped while paused must not leave the
                        // pause valves dropping the idle placeholder.
                        broadcast.resume();
                        self.set_sharing(SharingState::Idle);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "the share could not be stopped");
                        let _ = self.updates.send(PresenterUpdate::ShareFailed {
                            message: error.to_string(),
                        });
                    }
                },
                None => {
                    self.source = None;
                    self.set_sharing(SharingState::Idle);
                }
            },
            PresenterCommand::CloseRoom => {
                self.signaling.send(ClientMessage::RoomClose {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: new_request_id(),
                });
                if self.close_deadline.is_none() {
                    self.close_deadline = Some(Box::pin(tokio::time::sleep(CLOSE_TIMEOUT)));
                }
            }
            PresenterCommand::Leave => return Some(self.finish(PresenterEnd::Left)),
            PresenterCommand::ApproveViewer(peer_id) => {
                self.approvals_sent.insert(peer_id.clone());
                self.signaling.send(ClientMessage::ViewerApprove {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: new_request_id(),
                    peer_id,
                });
            }
            PresenterCommand::RejectViewer(peer_id) => {
                self.approvals_sent.remove(&peer_id);
                self.announced_requests.remove(&peer_id);
                self.signaling.send(ClientMessage::ViewerReject {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: new_request_id(),
                    peer_id,
                });
            }
            PresenterCommand::KickViewer(peer_id) => {
                self.signaling.send(ClientMessage::ViewerKick {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: new_request_id(),
                    peer_id,
                });
            }
            PresenterCommand::RestartIce(peer_id) => self.begin_recovery(peer_id),
            PresenterCommand::SetDisplayName(name) => {
                // The presenter's name lives only in the chat envelope; the
                // server tracks display names for viewers alone.
                self.display_name = name;
            }
        }
        None
    }

    /// Starts or switches the shared capture; replacing the capture head
    /// covers both, without touching viewer connections.
    fn share(&mut self, source: SourceConfig) {
        match &self.broadcast {
            Some(broadcast) => match broadcast.replace_source(source) {
                Ok(()) => {
                    // Going live always streams: a pause carried over from an
                    // earlier share (Pause -> Stop -> Start) would otherwise
                    // leave every viewer valve dropping while the room
                    // announces Live, with no ResumeShare able to clear it.
                    broadcast.resume();
                    self.set_sharing(SharingState::Live);
                }
                Err(error) => {
                    tracing::warn!(%error, "the capture could not be shared");
                    let _ = self.updates.send(PresenterUpdate::ShareFailed {
                        message: error.to_string(),
                    });
                }
            },
            // Not authenticated yet: hold the capture for the broadcast start.
            None => {
                self.source = Some(source);
                self.set_sharing(SharingState::Live);
            }
        }
    }

    fn handle_message(
        &mut self,
        message: ServerMessage,
    ) -> Result<Option<PresenterEnd>, PresenterError> {
        match message {
            ServerMessage::AuthSucceeded {
                peer_id,
                snapshot,
                ice_configuration,
                ..
            } => {
                let fresh_identity = self
                    .self_peer_id
                    .as_deref()
                    .is_some_and(|known| known != peer_id);
                self.self_peer_id = Some(peer_id);
                self.schedule_ice_refresh(&ice_configuration.expires_at);
                match &self.broadcast {
                    // Re-authentication after a signaling drop: the broadcast
                    // and every viewer connection keep running; only the ICE
                    // configuration is refreshed in case TURN credentials
                    // rotated. Media is never torn down for a signaling blip.
                    Some(broadcast) => {
                        broadcast.set_ice(&ice_configuration);
                        if fresh_identity {
                            // The resume window lapsed and this is a fresh
                            // authentication under a new peer id. Viewers
                            // still hold working media, so each is re-offered
                            // via an ICE restart to converge signaling routes
                            // on the new peer id.
                            let peers: Vec<String> = self.active_viewers.iter().cloned().collect();
                            self.recovering.clear();
                            for peer in peers {
                                self.begin_recovery(peer);
                            }
                        }
                    }
                    None => {
                        let source = self.source.take().unwrap_or(SourceConfig::Idle);
                        let (broadcast, events) = Broadcast::start(BroadcastConfig {
                            source,
                            audio: self.audio.clone(),
                            video_codecs: self.video_codecs.clone(),
                            frame_rate: self.frame_rate,
                            ice: ice_configuration,
                            force_relay: self.force_relay,
                            preview_frames: self.preview_frames.take(),
                            capture_ceiling: self.capture_ceiling,
                        })?;
                        for envelope in self.pending_chat.drain(..) {
                            broadcast.send_chat(&envelope);
                        }
                        self.broadcast = Some(broadcast);
                        self.broadcast_events = Some(events);
                    }
                }
                // Converge the server on this session's sharing state, then
                // report it so a GUI has the initial value.
                self.announce_sharing();
                let _ = self
                    .updates
                    .send(PresenterUpdate::SharingState(self.sharing));
                self.apply_snapshot(&snapshot);
                // A snapshot that outran auth:succeeded is newer than the auth
                // outcome's; apply it last so its membership wins.
                if let Some(buffered) = self.pending_snapshot.take() {
                    self.apply_snapshot(&buffered);
                }
            }
            ServerMessage::RoomSnapshot { snapshot, .. } => self.apply_snapshot(&snapshot),
            ServerMessage::ViewerPending { viewer, .. } => self.note_pending(&viewer),
            ServerMessage::ViewerDisplayNameUpdated {
                peer_id,
                display_name,
                ..
            } => {
                if let Some(broadcast) = &self.broadcast {
                    broadcast.set_viewer_display_name(&peer_id, display_name.as_deref());
                }
            }
            ServerMessage::ViewerLeft { peer_id, .. }
            | ServerMessage::ViewerKicked { peer_id, .. } => {
                self.drop_viewer(&peer_id);
            }
            ServerMessage::SignalAnswer {
                source_peer_id,
                sdp,
                ..
            } => {
                if let Some(broadcast) = &self.broadcast
                    && broadcast.accept_answer(&source_peer_id, &sdp).is_err()
                {
                    tracing::warn!("a viewer sent an unusable answer; awaiting renegotiation");
                }
            }
            ServerMessage::SignalIceCandidate {
                source_peer_id,
                candidate,
                sdp_m_line_index,
                ..
            } => {
                if let Some(broadcast) = &self.broadcast {
                    broadcast.add_remote_candidate(
                        &source_peer_id,
                        u32::from(sdp_m_line_index.unwrap_or(0)),
                        &candidate,
                    );
                }
            }
            ServerMessage::SignalIceRestart { source_peer_id, .. } => {
                // A viewer that detected its own connection failing asks for a
                // restart; drive it through the same escalating recovery.
                self.begin_recovery(source_peer_id);
            }
            ServerMessage::IceConfiguration { configuration, .. } => {
                self.schedule_ice_refresh(&configuration.expires_at);
                if let Some(broadcast) = &self.broadcast {
                    broadcast.set_ice(&configuration);
                }
            }
            ServerMessage::RoomClosed { .. } => {
                return Ok(Some(self.finish(PresenterEnd::RoomClosed)));
            }
            ServerMessage::RoomExpired { .. } => {
                return Ok(Some(self.finish(PresenterEnd::RoomExpired)));
            }
            ServerMessage::AuthFailed { message, .. } => {
                self.finish_broadcast();
                return Err(PresenterError::AuthenticationFailed(message));
            }
            ServerMessage::Error { code, message, .. } => match code {
                ErrorCode::RoomClosed => {
                    return Ok(Some(self.finish(PresenterEnd::RoomClosed)));
                }
                ErrorCode::RoomExpired => {
                    return Ok(Some(self.finish(PresenterEnd::RoomExpired)));
                }
                // A rejected message (a kick for a viewer that already left,
                // a replayed approval, a rate limit) must not end the room.
                _ => tracing::warn!(?code, %message, "the server rejected a message"),
            },
            _ => {}
        }
        Ok(None)
    }

    fn handle_broadcast(&mut self, event: BroadcastEvent) -> Result<(), PresenterError> {
        match event {
            BroadcastEvent::Chat { peer_id, text } => {
                match serde_json::from_str::<ChatMessage>(&text) {
                    Ok(chat) => {
                        let _ = self.updates.send(PresenterUpdate::Chat {
                            peer_id,
                            sender: chat.sender,
                            text: chat.text,
                        });
                    }
                    Err(_) => {
                        tracing::warn!(viewer = %peer_id, "dropping a chat payload that is not a ChatMessage envelope");
                    }
                }
            }
            BroadcastEvent::Offer {
                peer_id,
                sdp,
                ice_restart,
            } => {
                self.signaling.send(ClientMessage::SignalOffer {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: new_request_id(),
                    destination_peer_id: peer_id,
                    sdp,
                    ice_restart,
                });
            }
            BroadcastEvent::IceCandidate {
                peer_id,
                candidate,
                sdp_m_line_index,
            } => {
                self.signaling.send(ClientMessage::SignalIceCandidate {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: new_request_id(),
                    destination_peer_id: peer_id,
                    candidate,
                    sdp_mid: None,
                    sdp_m_line_index: Some(u16::try_from(sdp_m_line_index).unwrap_or(u16::MAX)),
                });
            }
            BroadcastEvent::ViewerConnection { peer_id, state } => {
                match state {
                    ConnectionState::Failed => self.begin_recovery(peer_id.clone()),
                    ConnectionState::Connected => {
                        self.recovering.remove(&peer_id);
                    }
                    _ => {}
                }
                let _ = self
                    .updates
                    .send(PresenterUpdate::ViewerConnection { peer_id, state });
            }
            BroadcastEvent::ViewerStats { peer_id, stats } => {
                let _ = self
                    .updates
                    .send(PresenterUpdate::ViewerStats { peer_id, stats });
            }
            BroadcastEvent::Ended { reason } => {
                self.finish_broadcast();
                self.signaling.disconnect(false);
                return Err(PresenterError::Broadcast(reason));
            }
        }
        Ok(())
    }

    fn encoder_settings(&self) -> EncoderSettings {
        EncoderSettings {
            bitrate_kbps: self.bitrate_kbps,
            adaptive: self.adaptive,
        }
    }

    /// Starts recovering one viewer: an ICE restart now, and a deadline after
    /// which the connection is rebuilt if it has not come back. A restart
    /// already in flight for the viewer is left to run rather than stacked.
    fn begin_recovery(&mut self, peer_id: String) {
        if !self.active_viewers.contains(&peer_id) || !self.recovering.insert(peer_id.clone()) {
            return;
        }
        if let Some(broadcast) = &self.broadcast {
            broadcast.restart_ice(&peer_id);
        }
        let deadline = self.recovery_deadlines.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RECOVERY_TIMEOUT).await;
            let _ = deadline.send(peer_id);
        });
    }

    /// Fires when a viewer's recovery deadline elapses. Still being in the
    /// recovering set means the ICE restart never reconnected it, so the
    /// connection is torn down and rebuilt from a fresh encoder and transport.
    fn rebuild_if_unrecovered(&mut self, peer_id: String) {
        if !self.recovering.remove(&peer_id) || !self.active_viewers.contains(&peer_id) {
            return;
        }
        let Some(broadcast) = &self.broadcast else {
            return;
        };
        tracing::warn!(viewer = %peer_id, "ICE restart did not recover; rebuilding the connection");
        broadcast.remove_viewer(&peer_id);
        if let Err(error) = broadcast.add_viewer(&peer_id, self.encoder_settings()) {
            tracing::warn!(%error, viewer = %peer_id, "the rebuilt connection could not be created");
            self.active_viewers.remove(&peer_id);
            let _ = self.updates.send(PresenterUpdate::ViewerLeft { peer_id });
        }
    }

    /// The snapshot is the authority on membership: every approved viewer gets
    /// a connection, and connections without an approved viewer are dropped.
    /// Pending viewers listed in it are surfaced as join requests, so
    /// requests made while the presenter was disconnected are not lost.
    fn apply_snapshot(&mut self, snapshot: &RoomSnapshot) {
        let _ = self.updates.send(PresenterUpdate::RoomExpiry {
            expires_in_seconds: snapshot.expires_in_seconds,
        });
        for viewer in &snapshot.pending_viewers {
            self.note_pending(viewer);
        }
        if self.broadcast.is_none() {
            self.pending_snapshot = Some(snapshot.clone());
            return;
        }
        let approved: HashSet<String> = snapshot
            .approved_viewers
            .iter()
            .map(|viewer| viewer.peer_id.clone())
            .collect();
        let settings = self.encoder_settings();
        for viewer in &snapshot.approved_viewers {
            let peer_id = &viewer.peer_id;
            // Relayed chat is stamped with the server-known name, so keep the
            // broadcast's map current even for viewers already connected.
            if let Some(broadcast) = &self.broadcast {
                broadcast.set_viewer_display_name(peer_id, viewer.display_name.as_deref());
            }
            if !self.active_viewers.insert(peer_id.clone()) {
                continue;
            }
            let Some(broadcast) = &self.broadcast else {
                break;
            };
            match broadcast.add_viewer(peer_id, settings) {
                Ok(()) => {
                    let _ = self.updates.send(PresenterUpdate::ViewerJoined {
                        peer_id: peer_id.clone(),
                        display_name: viewer.display_name.clone(),
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "a viewer connection could not be created");
                    self.active_viewers.remove(peer_id);
                }
            }
        }
        let departed: Vec<String> = self.active_viewers.difference(&approved).cloned().collect();
        for peer_id in departed {
            self.drop_viewer(&peer_id);
        }
    }

    /// Reports a join request once, and answers it immediately when
    /// auto-approval is on.
    fn note_pending(&mut self, viewer: &PeerSnapshot) {
        if self.auto_approve && self.approvals_sent.insert(viewer.peer_id.clone()) {
            self.signaling.send(ClientMessage::ViewerApprove {
                protocol_version: PROTOCOL_VERSION,
                request_id: new_request_id(),
                peer_id: viewer.peer_id.clone(),
            });
        }
        if self.announced_requests.insert(viewer.peer_id.clone()) {
            let _ = self.updates.send(PresenterUpdate::JoinRequested {
                peer_id: viewer.peer_id.clone(),
                display_name: viewer.display_name.clone(),
                friend_code: viewer.friend_code.clone(),
            });
        }
    }

    fn drop_viewer(&mut self, peer_id: &str) {
        self.approvals_sent.remove(peer_id);
        self.announced_requests.remove(peer_id);
        self.recovering.remove(peer_id);
        if !self.active_viewers.remove(peer_id) {
            return;
        }
        if let Some(broadcast) = &self.broadcast {
            broadcast.remove_viewer(peer_id);
        }
        let _ = self.updates.send(PresenterUpdate::ViewerLeft {
            peer_id: peer_id.to_owned(),
        });
    }

    /// Arms the credential-refresh timer for a configuration expiring at
    /// `expires_at`, replacing any earlier schedule.
    fn schedule_ice_refresh(&mut self, expires_at: &str) {
        self.ice_refresh = Some(Box::pin(tokio::time::sleep(ice_refresh_delay(expires_at))));
    }

    fn set_sharing(&mut self, state: SharingState) {
        self.sharing = state;
        self.announce_sharing();
        let _ = self.updates.send(PresenterUpdate::SharingState(state));
    }

    fn announce_sharing(&self) {
        self.signaling.send(ClientMessage::RoomUpdateSharingState {
            protocol_version: PROTOCOL_VERSION,
            request_id: new_request_id(),
            sharing_state: self.sharing,
        });
    }

    fn finish(&mut self, end: PresenterEnd) -> PresenterEnd {
        self.finish_broadcast();
        self.signaling.disconnect(false);
        end
    }

    fn finish_broadcast(&mut self) {
        self.broadcast_events = None;
        if let Some(broadcast) = self.broadcast.take() {
            broadcast.close();
        }
        self.active_viewers.clear();
        self.recovering.clear();
    }
}

/// Wraps outgoing chat in the protocol's [`ChatMessage`] envelope; the same
/// JSON shape the web client sends on the `chat` data channel.
pub(crate) fn chat_envelope(sender: &str, text: &str) -> String {
    serde_json::to_string(&ChatMessage {
        sender: sender.to_owned(),
        text: text.to_owned(),
    })
    .expect("chat messages always serialize")
}
