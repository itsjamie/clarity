use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clarity_protocol::IceConfiguration;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;
use tokio::sync::mpsc;

use crate::ice::ice_endpoints;
use crate::overlay::{NativeHandle, NativeVideoSurface};
use crate::stats::{self, StatsBaseline, StreamStats};

const STATS_INTERVAL: Duration = Duration::from_secs(2);

/// One decoded video frame in tightly-packed RGBA (`width * height * 4` bytes).
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// A shared slot holding the most recent decoded frame. When a
/// [`PlaybackConfig`] carries one, video is decoded into it (as RGBA) instead
/// of opening a window, so a GUI can upload it as a texture.
pub type FrameSink = Arc<Mutex<Option<VideoFrame>>>;

#[derive(Clone)]
pub struct PlaybackConfig {
    pub ice: IceConfiguration,
    /// Restrict ICE to relayed candidates. Diagnostic aid, matching the web
    /// client's forced-relay acceptance test; leave off for normal viewing.
    pub force_relay: bool,
    /// Where to deliver decoded video. `Some` renders into the slot as RGBA;
    /// `None` opens a native window (or a fakesink under `CLARITY_MEDIA_HEADLESS`).
    pub frames: Option<FrameSink>,
    /// Window handles for rendering video on a native Wayland subsurface,
    /// bypassing the texture path. Takes precedence over `frames` when the
    /// overlay can be built; on any failure `frames` is the fallback.
    pub native: Option<NativeHandle>,
}

/// State of the connection to the presenter, in WebRTC peer-connection terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    New,
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
}

/// State of ICE connectivity, reported separately from the overall connection
/// so diagnostics can distinguish transport recovery from session failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceState {
    New,
    Checking,
    Connected,
    Completed,
    Disconnected,
    Failed,
    Closed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackEvent {
    /// The SDP answer to relay to the presenter.
    Answer {
        sdp: String,
    },
    /// A local ICE candidate to relay to the presenter.
    IceCandidate {
        candidate: String,
        sdp_m_line_index: u32,
    },
    ConnectionState(ConnectionState),
    IceState(IceState),
    Stats(StreamStats),
    /// A chat message arrived from the presenter over the data channel.
    Chat {
        text: String,
    },
    /// Playback stopped and will not recover on its own: the pipeline failed,
    /// a decoder was unavailable, or the viewer closed the video window.
    Ended {
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    #[error("the media runtime could not be initialized: {0}")]
    Init(String),
    #[error(
        "the media component `{0}` is unavailable; install the GStreamer base, good, and bad plugin sets"
    )]
    MissingComponent(&'static str),
    #[error("the media pipeline could not be started: {0}")]
    Start(String),
    #[error("the presenter offer contained SDP the media stack could not parse")]
    InvalidSdp,
}

/// Receives one presenter's stream and renders it into a dedicated window.
///
/// Feed remote signals with [`accept_offer`](Self::accept_offer) and
/// [`add_remote_candidate`](Self::add_remote_candidate); everything Playback
/// produces — the answer, local candidates, state changes, statistics — arrives
/// on the event channel returned by [`start`](Self::start). Repeated offers
/// renegotiate the same connection, which is how presenter source changes and
/// ICE restarts arrive. After an `Ended` event the instance only ignores input;
/// recovery is a new `Playback`.
pub struct Playback {
    pipeline: gst::Pipeline,
    webrtc: gst::Element,
    shared: Arc<Shared>,
    shutdown: Arc<AtomicBool>,
    bus_thread: Option<JoinHandle<()>>,
}

struct Shared {
    events: mpsc::UnboundedSender<PlaybackEvent>,
    /// Remote candidates that arrived before the first remote description.
    /// Both fields are guarded by this one lock so a candidate can never race
    /// past the drain that follows `set-remote-description`.
    queued_candidates: Mutex<CandidateQueue>,
    /// The video size as of the most recently decoded frame, re-read from the
    /// decoded pad's caps per buffer so a mid-stream resolution change shows
    /// up in the next stats report.
    video_size: Mutex<Option<(u32, u32)>>,
    /// Every decoded video frame counts here; the stats reporter derives the
    /// measured frame rate from the counter's delta.
    frames_decoded: AtomicU64,
    fps_baseline: Mutex<Option<FpsBaseline>>,
    codec: Mutex<Option<String>>,
    volume_element: Mutex<Option<gst::Element>>,
    desired_volume: Mutex<f64>,
    stats_baseline: Mutex<Option<StatsBaseline>>,
    /// Decoded frames are written here when set; otherwise video opens a window.
    frames: Option<FrameSink>,
    /// The native Wayland overlay, when one was requested and could be built.
    /// Video renders through its sink in preference to `frames`.
    native: Option<Arc<NativeVideoSurface>>,
    /// The presenter-created chat channel, once it arrives via `on-data-channel`.
    chat_channel: Mutex<Option<gst_webrtc::WebRTCDataChannel>>,
    /// Chat sent before the channel is open, flushed on open. Mirrors the web
    /// viewer's queue-until-open behaviour so nothing typed during
    /// negotiation is lost.
    queued_chat: Mutex<Vec<String>>,
    ended: AtomicBool,
}

#[derive(Default)]
struct CandidateQueue {
    remote_description_set: bool,
    pending: Vec<(u32, String)>,
}

/// The frame counter reading a previous stats report was derived from.
struct FpsBaseline {
    at: Instant,
    frames: u64,
}

impl Playback {
    pub fn start(
        config: PlaybackConfig,
    ) -> Result<(Self, mpsc::UnboundedReceiver<PlaybackEvent>), PlaybackError> {
        ensure_gstreamer()?;
        for element in ["webrtcbin", "decodebin", "autovideosink", "autoaudiosink"] {
            if gst::ElementFactory::find(element).is_none() {
                return Err(PlaybackError::MissingComponent(element));
            }
        }

        let native = config.native.and_then(|handle| {
            let surface = NativeVideoSurface::create(handle).map(Arc::new);
            if surface.is_none() {
                tracing::warn!(
                    "the native video overlay could not be created; falling back to texture rendering"
                );
            }
            surface
        });

        let (events, receiver) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            events,
            queued_candidates: Mutex::new(CandidateQueue::default()),
            video_size: Mutex::new(None),
            frames_decoded: AtomicU64::new(0),
            fps_baseline: Mutex::new(None),
            codec: Mutex::new(None),
            volume_element: Mutex::new(None),
            desired_volume: Mutex::new(1.0),
            stats_baseline: Mutex::new(None),
            frames: config.frames.clone(),
            native,
            chat_channel: Mutex::new(None),
            queued_chat: Mutex::new(Vec::new()),
            ended: AtomicBool::new(false),
        });

        let pipeline = gst::Pipeline::new();
        let webrtc = gst::ElementFactory::make("webrtcbin")
            .name("webrtc")
            .property_from_str("bundle-policy", "max-bundle")
            .build()
            .map_err(|error| PlaybackError::Start(error.to_string()))?;
        if config.force_relay {
            webrtc.set_property_from_str("ice-transport-policy", "relay");
        }
        let endpoints = ice_endpoints(&config.ice);
        if let Some(stun) = endpoints.stun_server {
            webrtc.set_property("stun-server", &stun);
        }
        pipeline
            .add(&webrtc)
            .map_err(|error| PlaybackError::Start(error.to_string()))?;
        for turn in &endpoints.turn_servers {
            if !webrtc.emit_by_name::<bool>("add-turn-server", &[turn]) {
                tracing::warn!("a TURN server from the room configuration was not accepted");
            }
        }

        Self::wire_ice_candidates(&webrtc, &shared);
        Self::wire_connection_states(&webrtc, &shared);
        Self::wire_incoming_streams(&pipeline, &webrtc, &shared);
        Self::wire_data_channel(&webrtc, &shared);
        Self::wire_display_contexts(&pipeline, &shared);

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|error| PlaybackError::Start(error.to_string()))?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let bus_thread = spawn_bus_thread(&pipeline, &webrtc, &shared, &shutdown)?;

        Ok((
            Self {
                pipeline,
                webrtc,
                shared,
                shutdown,
                bus_thread: Some(bus_thread),
            },
            receiver,
        ))
    }

    /// Applies a presenter offer and produces an `Answer` event. Queued remote
    /// candidates are flushed once the offer is applied.
    pub fn accept_offer(&self, sdp: &str) -> Result<(), PlaybackError> {
        let message = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes())
            .map_err(|_| PlaybackError::InvalidSdp)?;
        let preferences = preferences_from_offer(&message, &renderable_encodings());
        let offer =
            gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Offer, message);
        let webrtc = self.webrtc.clone();
        let shared = Arc::clone(&self.shared);
        let applied = gst::Promise::with_change_func(move |reply| {
            if reply.is_err() {
                end(&shared, "the presenter offer could not be applied");
                return;
            }
            {
                let mut queue = shared.queued_candidates.lock().expect("candidate lock");
                queue.remote_description_set = true;
                for (mline, candidate) in queue.pending.drain(..) {
                    webrtc.emit_by_name::<()>("add-ice-candidate", &[&mline, &candidate]);
                }
            }
            apply_codec_preferences(&webrtc, &preferences);
            let answer_webrtc = webrtc.clone();
            let answer_shared = Arc::clone(&shared);
            let answered = gst::Promise::with_change_func(move |reply| {
                let answer = match reply {
                    Ok(Some(structure)) => structure
                        .get::<gst_webrtc::WebRTCSessionDescription>("answer")
                        .ok(),
                    _ => None,
                };
                let Some(answer) = answer else {
                    end(
                        &answer_shared,
                        "an answer to the presenter offer could not be created",
                    );
                    return;
                };
                answer_webrtc
                    .emit_by_name::<()>("set-local-description", &[&answer, &None::<gst::Promise>]);
                match answer.sdp().as_text() {
                    Ok(text) => {
                        let _ = answer_shared
                            .events
                            .send(PlaybackEvent::Answer { sdp: text });
                    }
                    Err(_) => end(&answer_shared, "the local answer could not be serialized"),
                }
            });
            webrtc.emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &answered]);
        });
        self.webrtc
            .emit_by_name::<()>("set-remote-description", &[&offer, &applied]);
        Ok(())
    }

    /// Sends a chat message to the presenter. Messages sent before the data
    /// channel is open are queued and flushed on open, so nothing typed
    /// during negotiation is lost.
    pub fn send_chat(&self, text: &str) {
        let channel = self.shared.chat_channel.lock().expect("chat lock").clone();
        match channel {
            Some(channel)
                if channel.ready_state() == gst_webrtc::WebRTCDataChannelState::Open =>
            {
                channel.send_string(Some(text));
            }
            _ => {
                self.shared
                    .queued_chat
                    .lock()
                    .expect("chat lock")
                    .push(text.to_owned());
            }
        }
    }

    /// Receives the presenter's chat channel and reports its messages. Only
    /// the channel labelled `chat` is adopted, matching the web viewer's
    /// label filter; anything else is ignored.
    fn wire_data_channel(webrtc: &gst::Element, shared: &Arc<Shared>) {
        let shared = Arc::clone(shared);
        webrtc.connect("on-data-channel", false, move |values| {
            let Ok(channel) = values[1].get::<gst_webrtc::WebRTCDataChannel>() else {
                return None;
            };
            if channel.label().as_deref() != Some(crate::broadcast::CHAT_CHANNEL_LABEL) {
                return None;
            }
            let events = shared.events.clone();
            channel.connect_on_message_string(move |_channel, message| {
                if let Some(text) = message {
                    let _ = events.send(PlaybackEvent::Chat {
                        text: text.to_owned(),
                    });
                }
            });
            let flush_shared = Arc::clone(&shared);
            channel.connect_on_open(move |channel| {
                let queued: Vec<String> = flush_shared
                    .queued_chat
                    .lock()
                    .expect("chat lock")
                    .drain(..)
                    .collect();
                for text in queued {
                    channel.send_string(Some(&text));
                }
            });
            *shared.chat_channel.lock().expect("chat lock") = Some(channel.clone());
            // The channel may already be open by the time it is announced;
            // flush anything queued rather than waiting for an open that
            // already happened.
            if channel.ready_state() == gst_webrtc::WebRTCDataChannelState::Open {
                let queued: Vec<String> = shared
                    .queued_chat
                    .lock()
                    .expect("chat lock")
                    .drain(..)
                    .collect();
                for text in queued {
                    channel.send_string(Some(&text));
                }
            }
            None
        });
    }

    /// Accepts a remote ICE candidate, in any order relative to the offer.
    pub fn add_remote_candidate(&self, sdp_m_line_index: u32, candidate: &str) {
        if candidate.trim().is_empty() {
            return;
        }
        let mut queue = self
            .shared
            .queued_candidates
            .lock()
            .expect("candidate lock");
        if queue.remote_description_set {
            self.webrtc
                .emit_by_name::<()>("add-ice-candidate", &[&sdp_m_line_index, &candidate]);
        } else {
            queue.pending.push((sdp_m_line_index, candidate.to_owned()));
        }
    }

    /// Sets playback volume in `0.0..=1.0`. Takes effect immediately, or as
    /// soon as the stream provides audio.
    pub fn set_volume(&self, level: f64) {
        let level = level.clamp(0.0, 1.0);
        *self.shared.desired_volume.lock().expect("volume lock") = level;
        if let Some(volume) = self
            .shared
            .volume_element
            .lock()
            .expect("volume lock")
            .as_ref()
        {
            volume.set_property("volume", level);
        }
    }

    pub fn close(mut self) {
        self.shutdown_internal();
    }

    fn shutdown_internal(&mut self) {
        // Repeated shutdown (close followed by drop) is a no-op.
        let Some(bus_thread) = self.bus_thread.take() else {
            return;
        };
        let webrtc = self.webrtc.clone();
        let pipeline = self.pipeline.clone();
        let shutdown = Arc::clone(&self.shutdown);
        // Tearing the pipeline down while ICE gathering is active races inside
        // the platform WebRTC stack and corrupts the heap (observed as
        // free(): invalid pointer in libnice). Let gathering settle first,
        // bounded so an unresponsive STUN server cannot stall shutdown — and
        // on a background thread, so close() and drop return promptly to the
        // caller (typically the GUI thread).
        crate::teardown::spawn_teardown("clarity-media-playback-teardown", move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while webrtc
                .property::<gst_webrtc::WebRTCICEGatheringState>("ice-gathering-state")
                == gst_webrtc::WebRTCICEGatheringState::Gathering
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            shutdown.store(true, Ordering::SeqCst);
            let _ = pipeline.set_state(gst::State::Null);
            let _ = bus_thread.join();
        });
    }

    fn wire_ice_candidates(webrtc: &gst::Element, shared: &Arc<Shared>) {
        let shared = Arc::clone(shared);
        webrtc.connect("on-ice-candidate", false, move |values| {
            let Ok(mline) = values[1].get::<u32>() else {
                return None;
            };
            let Ok(candidate) = values[2].get::<String>() else {
                return None;
            };
            if !candidate.trim().is_empty() {
                let _ = shared.events.send(PlaybackEvent::IceCandidate {
                    candidate,
                    sdp_m_line_index: mline,
                });
            }
            None
        });
    }

    fn wire_connection_states(webrtc: &gst::Element, shared: &Arc<Shared>) {
        let connection_shared = Arc::clone(shared);
        webrtc.connect_notify(Some("connection-state"), move |element, _| {
            use gst_webrtc::WebRTCPeerConnectionState as S;
            let state = match element.property::<S>("connection-state") {
                S::New => ConnectionState::New,
                S::Connecting => ConnectionState::Connecting,
                S::Connected => ConnectionState::Connected,
                S::Disconnected => ConnectionState::Disconnected,
                S::Failed => ConnectionState::Failed,
                S::Closed => ConnectionState::Closed,
                _ => return,
            };
            let _ = connection_shared
                .events
                .send(PlaybackEvent::ConnectionState(state));
        });
        let ice_shared = Arc::clone(shared);
        webrtc.connect_notify(Some("ice-connection-state"), move |element, _| {
            use gst_webrtc::WebRTCICEConnectionState as S;
            let state = match element.property::<S>("ice-connection-state") {
                S::New => IceState::New,
                S::Checking => IceState::Checking,
                S::Connected => IceState::Connected,
                S::Completed => IceState::Completed,
                S::Disconnected => IceState::Disconnected,
                S::Failed => IceState::Failed,
                S::Closed => IceState::Closed,
                _ => return,
            };
            let _ = ice_shared.events.send(PlaybackEvent::IceState(state));
        });
    }

    /// Answers `NeedContext` requests for the Wayland display handle. These
    /// must be answered synchronously — by the time the message reaches the
    /// async bus loop the sink has already given up and opened its own display
    /// connection — so this is a sync handler. It passes every other message
    /// through untouched, which keeps the existing `timed_pop` bus thread
    /// working as before.
    fn wire_display_contexts(pipeline: &gst::Pipeline, shared: &Arc<Shared>) {
        let Some(surface) = shared.native.clone() else {
            return;
        };
        let Some(bus) = pipeline.bus() else {
            return;
        };
        bus.set_sync_handler(move |_, message| {
            if let gst::MessageView::NeedContext(need) = message.view()
                && let Some(context) = surface.context_for(need.context_type())
                && let Some(element) = message
                    .src()
                    .and_then(|source| source.downcast_ref::<gst::Element>())
            {
                element.set_context(context);
                return gst::BusSyncReply::Drop;
            }
            gst::BusSyncReply::Pass
        });
    }

    /// The native overlay surface video renders on, when one exists. The GUI
    /// uses it to position the video inside the window.
    pub fn native_surface(&self) -> Option<Arc<NativeVideoSurface>> {
        self.shared.native.clone()
    }

    fn wire_incoming_streams(
        pipeline: &gst::Pipeline,
        webrtc: &gst::Element,
        shared: &Arc<Shared>,
    ) {
        let pipeline_weak = pipeline.downgrade();
        let shared = Arc::clone(shared);
        webrtc.connect_pad_added(move |_, pad| {
            if pad.direction() != gst::PadDirection::Src {
                return;
            }
            let Some(pipeline) = pipeline_weak.upgrade() else {
                return;
            };
            if let Some(caps) = pad.current_caps()
                && let Some(rtp) = caps.structure(0)
                && rtp.get::<&str>("media") == Ok("video")
                && let Ok(encoding) = rtp.get::<&str>("encoding-name")
            {
                *shared.codec.lock().expect("codec lock") = Some(encoding.to_owned());
            }
            if let Err(reason) = attach_stream(&pipeline, pad, &shared) {
                end(&shared, &reason);
            }
        });
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.shutdown_internal();
    }
}

/// Feeds one incoming RTP stream through `decodebin` into a rendering chain.
fn attach_stream(
    pipeline: &gst::Pipeline,
    pad: &gst::Pad,
    shared: &Arc<Shared>,
) -> Result<(), String> {
    let decode = gst::ElementFactory::make("decodebin")
        .build()
        .map_err(|error| error.to_string())?;
    {
        let pipeline_weak = pipeline.downgrade();
        let shared = Arc::clone(shared);
        decode.connect_pad_added(move |_, decoded_pad| {
            let Some(pipeline) = pipeline_weak.upgrade() else {
                return;
            };
            if let Err(reason) = attach_sink(&pipeline, decoded_pad, &shared) {
                end(&shared, &reason);
            }
        });
    }
    pipeline.add(&decode).map_err(|error| error.to_string())?;
    decode
        .sync_state_with_parent()
        .map_err(|error| error.to_string())?;
    let sink = decode
        .static_pad("sink")
        .ok_or("the decoder is missing its input")?;
    pad.link(&sink)
        .map_err(|_| "the incoming stream could not reach the decoder".to_owned())?;
    Ok(())
}

/// Renders one decoded stream: video into the playback window, audio through
/// the default output with a volume stage.
fn attach_sink(
    pipeline: &gst::Pipeline,
    pad: &gst::Pad,
    shared: &Arc<Shared>,
) -> Result<(), String> {
    let caps = pad
        .current_caps()
        .ok_or("a decoded stream arrived without a format")?;
    let media = caps
        .structure(0)
        .ok_or("a decoded stream arrived without a format")?;
    let elements: Vec<gst::Element> = if media.name().starts_with("video/") {
        let width = media.get::<i32>("width").ok();
        let height = media.get::<i32>("height").ok();
        if let (Some(width), Some(height)) = (width, height) {
            *shared.video_size.lock().expect("format lock") =
                Some((width.max(0) as u32, height.max(0) as u32));
        }
        // Truthful statistics: count every decoded frame and re-read the caps
        // it arrived under, so the reported resolution and frame rate are what
        // is actually being rendered — a presenter's mid-stream source change
        // included — not the one-shot values negotiation produced.
        let probe_shared = Arc::clone(shared);
        pad.add_probe(gst::PadProbeType::BUFFER, move |pad, _| {
            probe_shared.frames_decoded.fetch_add(1, Ordering::Relaxed);
            if let Some(caps) = pad.current_caps()
                && let Some(video) = caps.structure(0)
                && let (Ok(width), Ok(height)) =
                    (video.get::<i32>("width"), video.get::<i32>("height"))
            {
                *probe_shared.video_size.lock().expect("format lock") =
                    Some((width.max(0) as u32, height.max(0) as u32));
            }
            gst::PadProbeReturn::Ok
        });
        // The overlay's single waylandsink can serve one stream; a further
        // video stream on the same connection (renegotiation) falls back to
        // the texture or window path instead of failing the pipeline.
        let native_sink = shared
            .native
            .as_ref()
            .map(|surface| surface.sink())
            .filter(|sink| sink.parent().is_none());
        match (native_sink, &shared.frames) {
            (Some(native), _) => vec![
                build_element("queue")?,
                // Negotiation safety: waylandsink still takes NV12/I420
                // directly, making the convert element passthrough.
                build_element("videoconvert")?,
                native,
            ],
            (None, Some(sink)) => vec![
                build_element("queue")?,
                build_element("videoconvert")?,
                frame_appsink(sink.clone())?,
            ],
            (None, None) => ["queue", "videoconvert", video_sink_name()]
                .into_iter()
                .map(build_element)
                .collect::<Result<_, _>>()?,
        }
    } else if media.name().starts_with("audio/") {
        let chain = [
            "queue",
            "audioconvert",
            "audioresample",
            "volume",
            audio_sink_name(),
        ]
        .into_iter()
        .map(build_element)
        .collect::<Result<Vec<_>, _>>()?;
        let volume = chain[3].clone();
        volume.set_property(
            "volume",
            *shared.desired_volume.lock().expect("volume lock"),
        );
        *shared.volume_element.lock().expect("volume lock") = Some(volume);
        chain
    } else {
        return Ok(());
    };

    pipeline
        .add_many(&elements)
        .map_err(|error| error.to_string())?;
    gst::Element::link_many(&elements).map_err(|error| error.to_string())?;
    for element in elements.iter().rev() {
        element
            .sync_state_with_parent()
            .map_err(|error| error.to_string())?;
    }
    let first = elements
        .first()
        .and_then(|element| element.static_pad("sink"))
        .ok_or("the rendering chain is missing its input")?;
    pad.link(&first)
        .map_err(|_| "the decoded stream could not reach the renderer".to_owned())?;
    Ok(())
}

fn build_element(name: &'static str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| format!("the media component `{name}` is unavailable"))
}

/// An `appsink` that converts each frame to RGBA and stores the latest one in
/// `sink`, keeping only the newest so a slow UI never backs the pipeline up.
/// Shared with the broadcast's presenter self-preview branch.
pub(crate) fn frame_appsink(sink: FrameSink) -> Result<gst::Element, String> {
    let appsink = gst::ElementFactory::make("appsink")
        .build()
        .map_err(|_| "the media component `appsink` is unavailable".to_owned())?;
    let caps = gst::Caps::builder("video/x-raw").field("format", "RGBA").build();
    appsink.set_property("caps", &caps);
    appsink.set_property("emit-signals", true);
    appsink.set_property("sync", false);
    appsink.set_property("max-buffers", 1u32);
    appsink.set_property("drop", true);
    appsink.connect("new-sample", false, move |values| {
        let this = values[0].get::<gst::Element>().ok()?;
        let sample = this.emit_by_name::<gst::Sample>("pull-sample", &[]);
        if let Some(frame) = frame_from_sample(&sample) {
            *sink.lock().expect("frame lock") = Some(frame);
        }
        Some(gst::FlowReturn::Ok.to_value())
    });
    Ok(appsink)
}

/// Copies a raw RGBA sample into an owned [`VideoFrame`].
fn frame_from_sample(sample: &gst::Sample) -> Option<VideoFrame> {
    let buffer = sample.buffer()?;
    let structure = sample.caps()?.structure(0)?;
    let width = u32::try_from(structure.get::<i32>("width").ok()?).ok()?;
    let height = u32::try_from(structure.get::<i32>("height").ok()?).ok()?;
    let map = buffer.map_readable().ok()?;
    Some(VideoFrame {
        width,
        height,
        data: map.as_slice().to_vec(),
    })
}

/// `CLARITY_MEDIA_HEADLESS` decodes and consumes media without a window or
/// audio device, for tests and displayless environments.
fn headless() -> bool {
    std::env::var_os("CLARITY_MEDIA_HEADLESS").is_some()
}

fn video_sink_name() -> &'static str {
    if headless() {
        "fakesink"
    } else {
        "autovideosink"
    }
}

fn audio_sink_name() -> &'static str {
    if headless() {
        "fakesink"
    } else {
        "autoaudiosink"
    }
}

/// Codecs this installation can both depayload and decode. Both halves of the
/// chain are required: a decoder without its RTP depayloader (or vice versa)
/// still fails the pipeline.
fn renderable_encodings() -> std::collections::HashSet<&'static str> {
    let available = |name: &str| gst::ElementFactory::find(name).is_some();
    let candidates: [(&str, &str, &[&str]); 6] = [
        ("VP8", "rtpvp8depay", &["vp8dec"]),
        ("VP9", "rtpvp9depay", &["vp9dec"]),
        (
            "H264",
            "rtph264depay",
            &["vah264dec", "avdec_h264", "openh264dec"],
        ),
        (
            "H265",
            "rtph265depay",
            &["vah265dec", "nvh265dec", "avdec_h265", "libde265dec"],
        ),
        ("AV1", "rtpav1depay", &["vaav1dec", "dav1ddec", "av1dec"]),
        ("OPUS", "rtpopusdepay", &["opusdec"]),
    ];
    // Test hook: encodings listed in CLARITY_DECODE_DENY (comma-separated)
    // are treated as undecodable, standing in for a viewer without those
    // plugins so codec fallback is testable on a fully-equipped machine.
    let denied = std::env::var("CLARITY_DECODE_DENY").unwrap_or_default();
    let denied: Vec<String> = denied
        .split(',')
        .map(|name| name.trim().to_ascii_uppercase())
        .filter(|name| !name.is_empty())
        .collect();
    candidates
        .into_iter()
        .filter(|(encoding, depayloader, decoders)| {
            !denied.iter().any(|name| name == encoding)
                && available(depayloader)
                && decoders.iter().any(|decoder| available(decoder))
        })
        .map(|(encoding, _, _)| encoding)
        .collect()
}

/// Per-m-line transceiver codec preferences: the offer's codecs filtered to
/// what this installation can render, in the offer's own order. Codec choice
/// belongs to the presenter — the viewer only removes what it cannot decode,
/// so the answer never re-ranks the presenter's preference. Auxiliary RTP
/// mechanisms (retransmission and FEC) pass through when offered. `None` for
/// an m-line whose codecs are all unrenderable, leaving default negotiation to
/// fail with a diagnosable missing-plugin error rather than rejecting the
/// stream silently.
pub(crate) const TWCC_EXTENSION_URI: &str =
    "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01";

/// The transport-wide congestion control extension id offered on this m-line,
/// if the local RTP stack can serve it. Browsers only send transport-wide
/// feedback — the input to their fast bandwidth ramp — when the answer
/// negotiates this extension; `a=rtcp-fb transport-cc` alone is not enough.
fn offered_twcc_extension(media: &gst_sdp::SDPMediaRef) -> Option<String> {
    gst::ElementFactory::find("rtphdrexttwcc")?;
    media.attributes().find_map(|attribute| {
        if attribute.key() != "extmap" {
            return None;
        }
        let (id, uri) = attribute.value()?.split_once(' ')?;
        if uri.trim() != TWCC_EXTENSION_URI {
            return None;
        }
        // Strip an RFC 8285 direction suffix such as "2/recvonly".
        Some(id.split('/').next().unwrap_or(id).to_owned())
    })
}

fn preferences_from_offer(
    offer: &gst_sdp::SDPMessage,
    renderable: &std::collections::HashSet<&'static str>,
) -> Vec<Option<gst::Caps>> {
    const AUXILIARY: [&str; 3] = ["RTX", "RED", "ULPFEC"];
    offer
        .medias()
        .map(|media| {
            let media_kind = media.media()?.to_owned();
            let twcc_extension = offered_twcc_extension(media);
            let mut seen: Vec<String> = Vec::new();
            let mut codecs: Vec<gst::Structure> = Vec::new();
            let mut auxiliaries: Vec<gst::Structure> = Vec::new();
            for format in media.formats() {
                let Ok(payload_type) = format.parse::<i32>() else {
                    continue;
                };
                let Some(format_caps) = media.caps_from_media(payload_type) else {
                    continue;
                };
                let Some(rtp) = format_caps.structure(0) else {
                    continue;
                };
                let Ok(encoding) = rtp.get::<&str>("encoding-name") else {
                    continue;
                };
                let encoding = encoding.to_ascii_uppercase();
                let auxiliary = AUXILIARY.contains(&encoding.as_str());
                if seen.contains(&encoding)
                    || (!auxiliary && !renderable.contains(encoding.as_str()))
                {
                    continue;
                }
                let mut structure = gst::Structure::builder("application/x-rtp")
                    .field("media", &media_kind)
                    .field("encoding-name", &encoding);
                if let Ok(clock_rate) = rtp.get::<i32>("clock-rate") {
                    structure = structure.field("clock-rate", clock_rate);
                }
                if let Some(id) = &twcc_extension
                    && !auxiliary
                {
                    structure = structure.field(format!("extmap-{id}"), TWCC_EXTENSION_URI);
                }
                if auxiliary {
                    auxiliaries.push(structure.build());
                } else {
                    codecs.push(structure.build());
                }
                seen.push(encoding);
            }
            // Auxiliaries only make sense alongside a codec they can assist.
            if codecs.is_empty() {
                return None;
            }
            let mut caps = gst::Caps::new_empty();
            {
                let caps = caps.get_mut().expect("caps are not yet shared");
                for structure in codecs.into_iter().chain(auxiliaries) {
                    caps.append_structure(structure);
                }
            }
            Some(caps)
        })
        .collect()
}

fn apply_codec_preferences(webrtc: &gst::Element, preferences: &[Option<gst::Caps>]) {
    for index in 0..i32::try_from(preferences.len()).unwrap_or(0) {
        let Some(transceiver) = webrtc
            .emit_by_name::<Option<gst_webrtc::WebRTCRTPTransceiver>>("get-transceiver", &[&index])
        else {
            break;
        };
        let mline = transceiver.property::<u32>("mlineindex");
        if let Some(Some(caps)) = preferences.get(mline as usize) {
            transceiver.set_property("codec-preferences", caps);
        }
    }
}

fn spawn_bus_thread(
    pipeline: &gst::Pipeline,
    webrtc: &gst::Element,
    shared: &Arc<Shared>,
    shutdown: &Arc<AtomicBool>,
) -> Result<JoinHandle<()>, PlaybackError> {
    let bus = pipeline
        .bus()
        .ok_or_else(|| PlaybackError::Start("the pipeline has no message bus".into()))?;
    let webrtc = webrtc.clone();
    let shared = Arc::clone(shared);
    let shutdown = Arc::clone(shutdown);
    std::thread::Builder::new()
        .name("clarity-media-bus".into())
        .spawn(move || {
            let mut last_stats = Instant::now();
            // Human descriptions from `missing-plugin` messages; they precede
            // the generic pipeline error and name what to install.
            let mut missing_plugins: Vec<String> = Vec::new();
            while !shutdown.load(Ordering::SeqCst) {
                if let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(250)) {
                    use gst::MessageView;
                    match message.view() {
                        MessageView::Element(element) => {
                            if let Some(structure) = element.structure()
                                && structure.name() == "missing-plugin"
                                && let Ok(description) = structure.get::<String>("name")
                            {
                                missing_plugins.push(description);
                            }
                        }
                        MessageView::Error(error) => {
                            let mut reason = format!("media pipeline error: {}", error.error());
                            if !missing_plugins.is_empty() {
                                reason.push_str(&format!(
                                    " (missing: {})",
                                    missing_plugins.join(", ")
                                ));
                            } else if let Some(debug) = error.debug() {
                                reason.push_str(&format!(" ({debug})"));
                            }
                            end(&shared, &reason);
                            return;
                        }
                        MessageView::Eos(_) => {
                            end(&shared, "the media stream ended");
                            return;
                        }
                        _ => {}
                    }
                }
                if last_stats.elapsed() >= STATS_INTERVAL {
                    last_stats = Instant::now();
                    request_stats(&webrtc, &shared);
                }
            }
        })
        .map_err(|error| PlaybackError::Start(error.to_string()))
}

fn request_stats(webrtc: &gst::Element, shared: &Arc<Shared>) {
    let shared = Arc::clone(shared);
    let promise = gst::Promise::with_change_func(move |reply| {
        let Ok(Some(report)) = reply else {
            return;
        };
        let parsed = stats::parse_report(report);
        let bitrate = parsed.bytes_received.and_then(|bytes| {
            let mut baseline = shared.stats_baseline.lock().expect("stats lock");
            let bitrate = stats::bitrate_kbps(baseline.as_ref(), bytes);
            *baseline = Some(StatsBaseline {
                at: Instant::now(),
                bytes_received: bytes,
            });
            bitrate
        });
        let size = *shared.video_size.lock().expect("format lock");
        // The measured frame rate: decoded frames since the previous report,
        // over the elapsed time. Honest by construction — a paused or stalled
        // stream reports zero rather than the negotiated nominal rate.
        let frames = shared.frames_decoded.load(Ordering::Relaxed);
        let frames_per_second = {
            let mut baseline = shared.fps_baseline.lock().expect("fps lock");
            let fps = baseline.as_ref().and_then(|previous| {
                let elapsed = previous.at.elapsed().as_secs_f64();
                (elapsed > 0.0)
                    .then(|| frames.saturating_sub(previous.frames) as f64 / elapsed)
            });
            *baseline = Some(FpsBaseline {
                at: Instant::now(),
                frames,
            });
            fps
        };
        let _ = shared.events.send(PlaybackEvent::Stats(StreamStats {
            bitrate_kbps: bitrate,
            packets_lost: parsed.packets_lost,
            packets_received: parsed.packets_received,
            round_trip_ms: parsed.round_trip_ms,
            width: size.map(|(width, _)| width),
            height: size.map(|(_, height)| height),
            frames_per_second,
            codec: shared.codec.lock().expect("codec lock").clone(),
        }));
    });
    webrtc.emit_by_name::<()>("get-stats", &[&None::<gst::Pad>, &promise]);
}

fn end(shared: &Shared, reason: &str) {
    if !shared.ended.swap(true, Ordering::SeqCst) {
        let _ = shared.events.send(PlaybackEvent::Ended {
            reason: reason.to_owned(),
        });
    }
}

pub(crate) fn ensure_gstreamer() -> Result<(), PlaybackError> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| gst::init().map_err(|error| error.to_string()))
        .clone()
        .map_err(PlaybackError::Init)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFFER: &str = "v=0\r\n\
        o=- 0 0 IN IP4 127.0.0.1\r\n\
        s=-\r\n\
        t=0 0\r\n\
        m=video 9 UDP/TLS/RTP/SAVPF 45 98 96 97\r\n\
        a=rtpmap:45 AV1/90000\r\n\
        a=rtpmap:98 VP9/90000\r\n\
        a=rtpmap:96 VP8/90000\r\n\
        a=rtpmap:97 rtx/90000\r\n\
        m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
        a=rtpmap:111 opus/48000/2\r\n";

    fn encodings(caps: &gst::Caps) -> Vec<String> {
        caps.iter()
            .map(|s| s.get::<&str>("encoding-name").expect("has encoding").into())
            .collect()
    }

    #[test]
    fn preferences_follow_the_offer_order_and_drop_unrenderable_codecs() {
        if ensure_gstreamer().is_err() {
            return;
        }
        let offer = gst_sdp::SDPMessage::parse_buffer(OFFER.as_bytes()).expect("offer parses");

        // Everything renderable: the presenter's AV1-first order is preserved.
        let all = ["AV1", "VP9", "VP8", "OPUS"].into_iter().collect();
        let preferences = preferences_from_offer(&offer, &all);
        assert_eq!(preferences.len(), 2);
        assert_eq!(
            encodings(preferences[0].as_ref().expect("video caps")),
            ["AV1", "VP9", "VP8", "RTX"]
        );
        assert_eq!(
            encodings(preferences[1].as_ref().expect("audio caps")),
            ["OPUS"]
        );

        // AV1 unrenderable: it is filtered out, the rest keep the offer order.
        let without_av1 = ["VP9", "VP8", "OPUS"].into_iter().collect();
        let preferences = preferences_from_offer(&offer, &without_av1);
        assert_eq!(
            encodings(preferences[0].as_ref().expect("video caps")),
            ["VP9", "VP8", "RTX"]
        );

        // Nothing renderable on an m-line: no preference is imposed at all.
        let audio_only = ["OPUS"].into_iter().collect();
        let preferences = preferences_from_offer(&offer, &audio_only);
        assert!(preferences[0].is_none());
        assert!(preferences[1].is_some());
    }
}
