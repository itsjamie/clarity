//! The local viewing session for the GUI: join a friend's room and show its
//! live video in-app.
//!
//! [`ViewerLink`] runs a [`ViewerSession`] whose decoded frames land in a shared
//! [`FrameSink`]. Each frame the render loop calls [`pump`](ViewerLink::pump),
//! which uploads the newest frame to an egui texture and drains session events
//! into [`ViewerView`]. Session commands (volume, rename, manual ICE restart)
//! go the other way through [`command`](ViewerLink::command). Dropping the
//! link leaves the room.

use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use clarity_client::invite::parse_invitation;
use clarity_client::signaling::{SessionIdentity, SignalingState};
use clarity_client::viewer::{
    EndReason, ViewerCommand as SessionCommand, ViewerPhase, ViewerSession, ViewerSessionConfig,
    ViewerUpdate,
};
use clarity_client::{ConnectionState, FrameSink, NativeHandle, NativeVideoSurface};
use clarity_protocol::SharingState;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::state::{ChatMessage, RoomCountdown, ViewerView};

enum Event {
    Status(String),
    Live,
    Sharing(SharingState),
    Expiry(u64),
    PresenterConnected(bool),
    Reconnecting(bool),
    Stats {
        bitrate_kbps: Option<u32>,
        round_trip_ms: Option<f64>,
        packets_lost: Option<i64>,
        packets_received: Option<u64>,
        fps: Option<f64>,
        codec: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
    },
    Chat { sender: String, text: String },
    NativeSurface(Arc<NativeVideoSurface>),
    Ended(String),
}

/// What the joining side brings to the session: how this viewer introduces
/// itself and proves who it is.
pub struct ViewerConfig {
    /// The name the room and chat see.
    pub display_name: Option<String>,
    /// Proof material for friends-only rooms; `None` cannot join them.
    pub identity: Option<SessionIdentity>,
    /// Route media through the TURN relay even when a direct path exists.
    pub force_relay: bool,
}

pub struct ViewerLink {
    incoming: Receiver<Event>,
    commands: UnboundedSender<SessionCommand>,
    frames: FrameSink,
    /// The reused video texture; kept so frames update in place.
    texture: Option<egui::TextureHandle>,
    last_size: Option<(u32, u32)>,
    /// The native overlay surface, once playback reports one. Video then
    /// renders below the window and the texture path stays idle.
    surface: Option<Arc<NativeVideoSurface>>,
}

impl ViewerLink {
    /// Starts a viewing session for a room's viewer URL on the given runtime.
    /// `native` requests rendering on a Wayland subsurface below the window;
    /// the frame sink stays wired regardless, as the fallback path.
    pub fn start(
        runtime: &tokio::runtime::Handle,
        ctx: &egui::Context,
        viewer_url: &str,
        config: ViewerConfig,
        native: Option<NativeHandle>,
    ) -> Self {
        let (forward, incoming) = channel();
        let (commands, commands_rx) = unbounded_channel();
        let frames: FrameSink = Arc::new(Mutex::new(None));
        let repaint = ctx.clone();
        let emit = move |event: Event| {
            let _ = forward.send(event);
            repaint.request_repaint();
        };

        match parse_invitation(viewer_url) {
            Ok(invitation) => {
                let (updates, mut updates_rx) = tokio::sync::mpsc::unbounded_channel();
                let session = {
                    // The session spawns its signaling task, so it must be
                    // created inside the runtime's context.
                    let _guard = runtime.enter();
                    ViewerSession::start(
                        ViewerSessionConfig {
                            invitation,
                            display_name: config.display_name,
                            identity: config.identity,
                            force_relay: config.force_relay,
                            frames: Some(frames.clone()),
                            native,
                        },
                        updates,
                        commands_rx,
                    )
                };
                runtime.spawn(async move {
                    let run = tokio::spawn(session.run());
                    while let Some(update) = updates_rx.recv().await {
                        forward_update(update, &emit);
                    }
                    let reason = match run.await {
                        Ok(Ok(end)) => end_label(end).to_owned(),
                        Ok(Err(error)) => format!("The stream ended: {error}"),
                        Err(_) => "The viewing task ended unexpectedly.".to_owned(),
                    };
                    emit(Event::Ended(reason));
                });
            }
            Err(error) => {
                emit(Event::Ended(format!("That room link is not valid: {error}")));
            }
        }

        Self {
            incoming,
            commands,
            frames,
            texture: None,
            last_size: None,
            surface: None,
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

    /// Drains events into `view` and uploads the newest decoded frame as a
    /// texture. Cheap to call every frame.
    pub fn pump(&mut self, ctx: &egui::Context, view: &mut ViewerView) {
        while let Ok(event) = self.incoming.try_recv() {
            match event {
                Event::Status(status) => view.status = status,
                Event::Live => {
                    view.live = true;
                    view.reconnecting = false;
                    view.status = "Live".to_owned();
                }
                Event::Sharing(sharing) => view.sharing = Some(sharing),
                Event::Expiry(seconds) => {
                    view.countdown = Some(RoomCountdown {
                        seconds,
                        at: std::time::Instant::now(),
                    });
                }
                Event::PresenterConnected(connected) => view.presenter_connected = connected,
                Event::Reconnecting(reconnecting) => view.reconnecting = reconnecting,
                Event::Stats {
                    bitrate_kbps,
                    round_trip_ms,
                    packets_lost,
                    packets_received,
                    fps,
                    codec,
                    width,
                    height,
                } => {
                    view.bitrate_kbps = bitrate_kbps;
                    view.round_trip_ms = round_trip_ms;
                    view.packets_lost = packets_lost;
                    view.packets_received = packets_received;
                    view.fps = fps;
                    view.codec = codec;
                    view.bitrate_history.record(bitrate_kbps.unwrap_or(0));
                    // On the native path no decoded frames cross the app, so the
                    // stream dimensions (which size the transparent hole) come
                    // from stats rather than the texture upload below.
                    if let (Some(w), Some(h)) = (width, height)
                        && w > 0
                        && h > 0
                    {
                        view.frame_size = Some((w, h));
                    }
                }
                Event::Chat { sender, text } => view.messages.push(ChatMessage {
                    from: sender,
                    text,
                    own: false,
                }),
                Event::NativeSurface(surface) => {
                    self.surface = Some(surface);
                    view.native_video = true;
                }
                Event::Ended(reason) => {
                    view.live = false;
                    view.reconnecting = false;
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
                    self.texture =
                        Some(ctx.load_texture("viewer-video", image, egui::TextureOptions::LINEAR));
                }
            }
            self.last_size = Some(dims);
            view.frame_size = Some(dims);
            view.texture = self.texture.clone();
        }

        // Frames arrive off the pipeline without touching egui, so keep the
        // render loop running while video plays; otherwise the view only
        // refreshes on the occasional stats event and looks choppy. The loop
        // self-sustains at the display's refresh and stops when playback ends.
        // On the native path the compositor presents frames itself, so egui
        // must not repaint per frame — that independence is the point.
        if view.live && !view.native_video {
            ctx.request_repaint();
        }
    }

    /// Places the native video subsurface under the stage's transparent hole,
    /// in logical points relative to the window's top-left. The surface caches
    /// the rectangle, so calling this every frame is cheap. `None` (no hole
    /// laid out) leaves the last rectangle: opaque chrome covers the video.
    pub fn sync_stage(&self, rect: Option<egui::Rect>) {
        let (Some(surface), Some(rect)) = (&self.surface, rect) else {
            return;
        };
        surface.set_rect(
            rect.left().round() as i32,
            rect.top().round() as i32,
            rect.width().round() as i32,
            rect.height().round() as i32,
        );
    }
}

fn forward_update(update: ViewerUpdate, emit: &impl Fn(Event)) {
    match update {
        ViewerUpdate::Phase(ViewerPhase::Live) => emit(Event::Live),
        ViewerUpdate::Phase(phase) => emit(Event::Status(phase_label(phase).to_owned())),
        ViewerUpdate::SharingState(sharing) => emit(Event::Sharing(sharing)),
        ViewerUpdate::RoomExpiry { expires_in_seconds } => {
            emit(Event::Expiry(expires_in_seconds));
        }
        ViewerUpdate::Connection(state) => {
            // The session already drives recovery (automatic ICE restarts);
            // this only tells the stage to say so.
            emit(Event::Reconnecting(matches!(
                state,
                ConnectionState::Disconnected | ConnectionState::Failed
            )));
        }
        ViewerUpdate::Stats(stats) => emit(Event::Stats {
            bitrate_kbps: stats.bitrate_kbps,
            round_trip_ms: stats.round_trip_ms,
            packets_lost: stats.packets_lost,
            packets_received: stats.packets_received,
            fps: stats.frames_per_second,
            codec: stats.codec,
            width: stats.width,
            height: stats.height,
        }),
        ViewerUpdate::Chat { sender, text } => emit(Event::Chat { sender, text }),
        ViewerUpdate::NativeSurface(surface) => emit(Event::NativeSurface(surface)),
        ViewerUpdate::PresenterConnected(connected) => {
            emit(Event::PresenterConnected(connected));
        }
        ViewerUpdate::Signaling(SignalingState::Reconnecting) => {
            emit(Event::Status("Reconnecting to the room…".to_owned()));
        }
        _ => {}
    }
}

fn phase_label(phase: ViewerPhase) -> &'static str {
    match phase {
        ViewerPhase::Connecting => "Connecting…",
        ViewerPhase::AwaitingApproval => "Waiting to be let in…",
        ViewerPhase::Negotiating => "Connecting to the stream…",
        ViewerPhase::Live => "Live",
    }
}

fn end_label(end: EndReason) -> &'static str {
    match end {
        EndReason::RoomClosed => "The presenter closed the room.",
        EndReason::RoomExpired => "The room expired.",
        EndReason::Rejected => "The presenter didn't let you in.",
        EndReason::Kicked => "You were removed from the room.",
        EndReason::Left => "You left the room.",
    }
}
