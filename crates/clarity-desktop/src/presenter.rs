//! The local presenter session for the GUI: create a room, host it (idle or
//! sharing), and surface progress, join requests, and per-viewer stats onto
//! the egui thread.
//!
//! [`PresenterLink`] owns one async task that runs the whole lifecycle. Events
//! are forwarded through a plain channel (with a repaint) and drained into
//! [`PresenterView`] by the render loop. Session commands flow the other way
//! through [`command`](PresenterLink::command). Dropping the link detaches
//! from the room without closing it; only an explicit
//! [`PresenterCommand::CloseRoom`](SessionCommand::CloseRoom) ends the room
//! for everyone.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use clarity_client::presenter::{
    PresenterCommand as SessionCommand, PresenterEnd, PresenterSession, PresenterSessionConfig,
    PresenterUpdate,
};
use clarity_client::rooms::{RoomOptions, create_room, server_endpoints};
use clarity_client::signaling::SignalingState;
use clarity_client::{
    AudioCapture, CaptureRequest, CaptureStream, ConnectionState, FrameSink, SourceConfig,
    SyntheticSource, VideoCodecId,
};
use clarity_identity::{CaptureProfile, Settings};
use clarity_protocol::{RoomAccessPolicy, SharingState};
use secrecy::SecretString;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use url::Url;

use crate::state::{ChatMessage, JoinRequest, PresenterView, RoomCountdown, ViewerCard};

/// What the presenter shares.
#[derive(Clone, Copy)]
pub enum Source {
    /// The real desktop portal picker (a monitor or window).
    Screen,
    /// A local test pattern, for verifying the pipeline without the picker.
    Synthetic,
}

/// Everything needed to open a room, gathered from settings and the create
/// modal.
pub struct PresenterConfig {
    pub server: String,
    pub profile: CaptureProfile,
    /// The largest frame (width, height) fed to the encoders, from the
    /// Settings "Max capture" choice.
    pub max_capture: (u32, u32),
    pub include_system_audio: bool,
    pub force_relay: bool,
    /// Ranked codec preference from Settings, parsed to engine ids; empty
    /// means the engine's default order.
    pub video_codecs: Vec<VideoCodecId>,
    pub access_policy: RoomAccessPolicy,
    /// Friend codes admitted to a friends-only room; ignored otherwise.
    pub allowed_friend_codes: Vec<String>,
    /// Admit every join request without asking ("Ask me first" turns this
    /// off, surfacing each request in the room UI).
    pub auto_approve: bool,
    pub expires_in_seconds: u32,
    /// The name outgoing chat carries.
    pub display_name: Option<String>,
    /// Start sharing this the moment the room opens; `None` opens it idle.
    pub initial_source: Option<Source>,
}

impl PresenterConfig {
    /// Builds the settings-derived half of a config; access policy and expiry
    /// come from the create modal.
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            server: settings.signaling_server.clone(),
            profile: settings.capture_profile,
            max_capture: settings.max_capture_dimensions(),
            include_system_audio: settings.include_system_audio,
            force_relay: settings.always_relay,
            video_codecs: settings
                .codec_ranking
                .iter()
                .filter_map(|id| VideoCodecId::parse(id))
                .collect(),
            access_policy: RoomAccessPolicy::Public,
            allowed_friend_codes: Vec::new(),
            auto_approve: true,
            expires_in_seconds: 2 * 60 * 60,
            display_name: None,
            initial_source: None,
        }
    }
}

/// Events from the presenter task to the UI.
enum Event {
    Status(String),
    Opened {
        room_id: String,
        viewer_url: String,
        presenter_secret: SecretString,
    },
    Sharing(SharingState),
    Expiry(u64),
    Reconnecting(bool),
    JoinRequested(JoinRequest),
    ViewerJoined { peer_id: String, name: Option<String> },
    ViewerLeft { peer_id: String },
    ViewerConnection { peer_id: String, connected: bool },
    ViewerStats(String, ViewerCard),
    Chat { sender: String, text: String },
    ShareFailed(String),
    Ended(String),
}

pub struct PresenterLink {
    incoming: Receiver<Event>,
    commands: UnboundedSender<SessionCommand>,
    /// For the picker tasks `share` spawns, and their failure events.
    runtime: tokio::runtime::Handle,
    forward: Sender<Event>,
    repaint: egui::Context,
    profile: CaptureProfile,
    /// The self-preview sink the broadcast writes captured frames into.
    frames: FrameSink,
    /// The reused preview texture; kept so frames update in place.
    texture: Option<egui::TextureHandle>,
    last_size: Option<(u32, u32)>,
}

impl PresenterLink {
    /// Starts the presenter lifecycle on the given runtime. Always returns a
    /// link; failures (bad server, room refused) arrive as an `Ended` status
    /// through [`pump`](Self::pump).
    pub fn start(
        runtime: &tokio::runtime::Handle,
        ctx: &egui::Context,
        config: PresenterConfig,
    ) -> Self {
        let (forward, incoming) = channel();
        let (commands, commands_rx) = unbounded_channel();
        let frames: FrameSink = Arc::new(Mutex::new(None));
        let profile = config.profile;
        let emit = {
            let forward = forward.clone();
            let repaint = ctx.clone();
            move |event: Event| {
                let _ = forward.send(event);
                repaint.request_repaint();
            }
        };
        runtime.spawn(run(config, commands_rx, frames.clone(), emit));
        Self {
            incoming,
            commands,
            runtime: runtime.clone(),
            forward,
            repaint: ctx.clone(),
            profile,
            frames,
            texture: None,
            last_size: None,
        }
    }

    /// Sends a command to the running session. Silently dropped once the
    /// session has ended.
    pub fn command(&self, command: SessionCommand) {
        let _ = self.commands.send(command);
    }

    pub fn chat(&self, text: String) {
        self.command(SessionCommand::Chat(text));
    }

    /// Opens the capture picker and shares the chosen source into the room —
    /// both the first share and a mid-stream source change, since the session
    /// swaps the capture head either way. A cancelled or failed picker leaves
    /// the broadcast in its previous state and reports why.
    pub fn share(&self, source: Source) {
        let commands = self.commands.clone();
        let forward = self.forward.clone();
        let repaint = self.repaint.clone();
        let profile = self.profile;
        self.runtime.spawn(async move {
            let _ = forward.send(Event::Status("Choose what to share…".to_owned()));
            repaint.request_repaint();
            match open_source(source, profile).await {
                Ok(source) => {
                    let _ = commands.send(SessionCommand::StartShare(source));
                }
                Err(reason) => {
                    let _ = forward.send(Event::ShareFailed(reason));
                }
            }
            repaint.request_repaint();
        });
    }

    /// Drains presenter events into `view` and uploads the newest captured
    /// frame as the self-preview texture. Cheap to call every frame.
    pub fn pump(&mut self, ctx: &egui::Context, view: &mut PresenterView) {
        while let Ok(event) = self.incoming.try_recv() {
            match event {
                Event::Status(status) => view.status = status,
                Event::Opened {
                    room_id,
                    viewer_url,
                    presenter_secret,
                } => {
                    view.open = true;
                    view.status = "Room open".to_owned();
                    view.room_id = Some(room_id);
                    view.viewer_url = Some(viewer_url);
                    view.presenter_secret = Some(presenter_secret);
                }
                Event::Sharing(sharing) => {
                    view.sharing = sharing;
                    view.status = match sharing {
                        SharingState::Idle => "Room open — not sharing".to_owned(),
                        SharingState::Live => "Sharing your screen".to_owned(),
                        SharingState::Paused => "Sharing paused".to_owned(),
                    };
                }
                Event::Expiry(seconds) => {
                    view.countdown = Some(RoomCountdown {
                        seconds,
                        at: std::time::Instant::now(),
                    });
                }
                Event::Reconnecting(reconnecting) => view.reconnecting = reconnecting,
                Event::JoinRequested(request) => {
                    if !view.requests.iter().any(|r| r.peer_id == request.peer_id) {
                        view.requests.push(request);
                    }
                }
                Event::ViewerJoined { peer_id, name } => {
                    view.requests.retain(|r| r.peer_id != peer_id);
                    let card = view.viewers.entry(peer_id).or_default();
                    card.name = name.unwrap_or_else(|| "Guest".to_owned());
                    card.connected = true;
                }
                Event::ViewerLeft { peer_id } => {
                    view.requests.retain(|r| r.peer_id != peer_id);
                    view.viewers.remove(&peer_id);
                }
                Event::ViewerConnection { peer_id, connected } => {
                    view.viewers.entry(peer_id).or_default().connected = connected;
                }
                Event::ViewerStats(peer_id, stats) => {
                    let card = view.viewers.entry(peer_id).or_default();
                    let name = std::mem::take(&mut card.name);
                    let connected = card.connected;
                    *card = ViewerCard {
                        name: if name.is_empty() { "Guest".to_owned() } else { name },
                        connected,
                        ..stats
                    };
                    let total: u32 = view
                        .viewers
                        .values()
                        .filter_map(|viewer| viewer.bitrate_kbps)
                        .sum();
                    view.bitrate_history.record(total);
                }
                Event::Chat { sender, text } => view.messages.push(ChatMessage {
                    from: sender,
                    text,
                    own: false,
                }),
                Event::ShareFailed(message) => view.status = message,
                Event::Ended(reason) => {
                    view.sharing = SharingState::Idle;
                    view.ended = true;
                    view.status = reason;
                }
            }
        }

        let frame = self.frames.lock().expect("frame lock").take();
        if let Some(frame) = frame {
            let size = [frame.width as usize, frame.height as usize];
            let image = egui::ColorImage::from_rgba_unmultiplied(size, &frame.data);
            let dims = (frame.width, frame.height);
            match &mut self.texture {
                Some(texture) if self.last_size == Some(dims) => {
                    texture.set(image, egui::TextureOptions::LINEAR);
                }
                _ => {
                    self.texture = Some(ctx.load_texture(
                        "presenter-preview",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
            self.last_size = Some(dims);
            view.frame_size = Some(dims);
            view.texture = self.texture.clone();
        }

        // Preview frames arrive off the pipeline without touching egui, so keep
        // the render loop running while sharing; otherwise the self-preview only
        // refreshes on the occasional viewer-stats event and looks choppy.
        if view.is_live() && !view.ended {
            ctx.request_repaint();
        }
    }
}

async fn run(
    config: PresenterConfig,
    commands: tokio::sync::mpsc::UnboundedReceiver<SessionCommand>,
    preview_frames: FrameSink,
    emit: impl Fn(Event),
) {
    let Ok(server) = Url::parse(&config.server) else {
        emit(Event::Ended("The signaling server URL is not valid.".to_owned()));
        return;
    };

    emit(Event::Status("Creating room…".to_owned()));
    let room = match create_room(
        &server,
        RoomOptions {
            maximum_viewers: 10,
            expires_in_seconds: config.expires_in_seconds,
            access_policy: config.access_policy,
            allowed_friend_codes: config.allowed_friend_codes.clone(),
        },
    )
    .await
    {
        Ok(room) => room,
        Err(error) => {
            emit(Event::Ended(format!("Could not create the room: {error}")));
            return;
        }
    };
    let Ok(endpoints) = server_endpoints(&server) else {
        emit(Event::Ended("The signaling server URL is not valid.".to_owned()));
        return;
    };
    let presenter_secret = SecretString::from(room.presenter_secret);
    emit(Event::Opened {
        room_id: room.room_id.clone(),
        viewer_url: room.viewer_url.clone(),
        presenter_secret: presenter_secret.clone(),
    });

    // A dev/screenshot aid may share immediately; the normal flow opens idle
    // and shares from the room UI.
    let source = match config.initial_source {
        Some(kind) => match open_source(kind, config.profile).await {
            Ok(source) => Some(source),
            Err(reason) => {
                emit(Event::ShareFailed(reason));
                None
            }
        },
        None => None,
    };

    let (updates, mut updates_rx) = tokio::sync::mpsc::unbounded_channel::<PresenterUpdate>();
    let session = PresenterSession::start(
        PresenterSessionConfig {
            room_id: room.room_id,
            presenter_secret,
            signaling_url: endpoints.signaling_url,
            origin: endpoints.origin,
            source,
            audio: if config.include_system_audio {
                AudioCapture::SystemMix
            } else {
                AudioCapture::Disabled
            },
            video_codecs: config.video_codecs.clone(),
            frame_rate: config.profile.fps(),
            capture_ceiling: Some(config.max_capture),
            bitrate_kbps: bitrate_for(config.profile),
            adaptive: true,
            force_relay: config.force_relay,
            preview_frames: Some(preview_frames),
            display_name: config.display_name,
            auto_approve: config.auto_approve,
        },
        updates,
        commands,
    );

    let session = tokio::spawn(session.run());
    while let Some(update) = updates_rx.recv().await {
        forward_update(update, &emit);
    }
    let reason = match session.await {
        Ok(Ok(PresenterEnd::RoomClosed)) => "The room was closed.".to_owned(),
        Ok(Ok(PresenterEnd::RoomExpired)) => "The room expired.".to_owned(),
        Ok(Ok(PresenterEnd::Left)) => "You left the room.".to_owned(),
        Ok(Err(error)) => format!("The room ended: {error}"),
        Err(_) => "The room task ended unexpectedly.".to_owned(),
    };
    emit(Event::Ended(reason));
}

fn forward_update(update: PresenterUpdate, emit: &impl Fn(Event)) {
    match update {
        PresenterUpdate::Signaling(state) => {
            emit(Event::Reconnecting(state == SignalingState::Reconnecting));
        }
        PresenterUpdate::SharingState(state) => emit(Event::Sharing(state)),
        PresenterUpdate::RoomExpiry { expires_in_seconds } => {
            emit(Event::Expiry(expires_in_seconds));
        }
        PresenterUpdate::JoinRequested {
            peer_id,
            display_name,
            friend_code,
        } => emit(Event::JoinRequested(JoinRequest {
            peer_id,
            name: display_name,
            friend_code,
        })),
        PresenterUpdate::ViewerJoined { peer_id, display_name } => emit(Event::ViewerJoined {
            peer_id,
            name: display_name,
        }),
        PresenterUpdate::ViewerLeft { peer_id } => emit(Event::ViewerLeft { peer_id }),
        PresenterUpdate::ViewerConnection { peer_id, state } => emit(Event::ViewerConnection {
            peer_id,
            connected: state == ConnectionState::Connected,
        }),
        PresenterUpdate::ViewerStats { peer_id, stats } => emit(Event::ViewerStats(
            peer_id,
            ViewerCard {
                connected: true,
                bitrate_kbps: stats.bitrate_kbps,
                round_trip_ms: stats.round_trip_ms,
                packets_lost: stats.packets_lost,
                packets_sent: stats.packets_sent,
                target_kbps: stats.target_kbps,
                codec: stats.codec,
                ..ViewerCard::default()
            },
        )),
        PresenterUpdate::Chat { sender, text, .. } => emit(Event::Chat { sender, text }),
        PresenterUpdate::ShareFailed { message } => {
            emit(Event::ShareFailed(format!("Sharing failed: {message}")));
        }
    }
}

async fn open_source(source: Source, profile: CaptureProfile) -> Result<SourceConfig, String> {
    match source {
        Source::Synthetic => Ok(SourceConfig::Synthetic(SyntheticSource {
            width: 1280,
            height: 720,
            frame_rate: profile.fps(),
        })),
        Source::Screen => CaptureStream::open(CaptureRequest {
            show_cursor: true,
            restore_token: None,
            remember: false,
        })
        .await
        .map(SourceConfig::Screen)
        .map_err(|error| format!("Screen capture did not start: {error}")),
    }
}

/// The adaptive bitrate ceiling per profile. Generous headroom so high-detail
/// screen content (small text at high resolution) stays crisp; the congestion
/// estimator scales the actual rate down when the link cannot sustain it.
pub fn bitrate_for(profile: CaptureProfile) -> u32 {
    match profile {
        CaptureProfile::Text => 6_000,
        CaptureProfile::Motion => 12_000,
    }
}
