use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clarity_protocol::{ChatMessage, IceConfiguration};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;
use tokio::sync::mpsc;

use crate::capture::CaptureStream;
use crate::ice::{IceEndpoints, ice_endpoints};
use crate::playback::{ConnectionState, FrameSink, TWCC_EXTENSION_URI, frame_appsink};
use crate::rate::AdaptiveController;
use crate::stats::{self, SenderStats, StatsBaseline};

const STATS_INTERVAL: Duration = Duration::from_secs(2);
/// The idle placeholder's frame size and rate: small and slow, but still a
/// live stream, so RTP, transport feedback, and the preview stay alive while
/// nothing is shared.
const IDLE_WIDTH: i32 = 640;
const IDLE_HEIGHT: i32 = 360;
const IDLE_FRAME_RATE: i32 = 4;
/// The fixed Opus bitrate reserved for audio on each viewer's transport; the
/// congestion estimate covers the whole transport, so this is subtracted
/// before the remainder is given to the video encoder.
const AUDIO_BITRATE_BPS: u32 = 128_000;
/// A floor that keeps screen content legible under mild congestion. The
/// estimator can still drop here, but not to the point where text is unreadable.
const VIDEO_MIN_KBPS: u32 = 600;

/// The starting video rate for an adaptive viewer. Deliberately high — near the
/// ceiling — because the Google congestion estimator cannot measure capacity
/// above the rate actually being sent. Screen content is often low-complexity,
/// so a low start makes the CBR encoder send little, the estimator reads that as
/// the link's ceiling, and the rate stays stuck low (the ~800 kbps trap).
/// Starting near the ceiling makes NVENC pad to a high rate, so the estimator
/// sees real throughput, confirms headroom, and only backs off on genuine
/// congestion. The first burst is capped so a very high ceiling does not flood a
/// constrained link on connect; the estimator ramps the rest of the way.
fn start_video_kbps(ceiling_kbps: u32) -> u32 {
    (ceiling_kbps * 3 / 4).clamp(VIDEO_MIN_KBPS, ceiling_kbps.min(6_000))
}

/// Sets an NVENC encoder's `max-bitrate` (the VBR peak) in kbit/sec, when the
/// element exposes the property. Older or software encoders that lack it keep
/// their single-rate behaviour.
fn set_max_bitrate(encoder: &gst::Element, kbps: u32) {
    if encoder.find_property("max-bitrate").is_some() {
        encoder.set_property("max-bitrate", kbps);
    }
}

/// The presenter's video codec choice. `Auto` picks the best available codec,
/// preferring quality (hardware AV1, then hardware H.264, then software VP8);
/// the rest force a specific codec and fall back only if its encoder is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoCodecPreference {
    #[default]
    Auto,
    /// Hardware H.264 (NVENC) — the most widely decodable WebRTC video codec.
    H264,
    /// Hardware AV1 (NVENC) — best detail per bit; narrower browser support.
    Av1,
    /// Software VP8 — universal but CPU-bound at high resolution.
    Vp8,
}

/// The resolved codec each viewer is encoded with. Hardware NVENC (H.264 or
/// AV1) holds a steady CBR bitrate without CPU cost, which software VP8 cannot
/// at high resolutions; VP8 is the fallback when no hardware encoder is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoCodec {
    H264Nvenc,
    Av1Nvenc,
    Vp8,
}

impl VideoCodec {
    /// Resolves a preference to the codec actually used, honoring the request
    /// where its encoder is installed and falling back through H.264 to VP8.
    fn select(preference: VideoCodecPreference) -> Self {
        let candidates: &[Self] = match preference {
            VideoCodecPreference::Vp8 => &[Self::Vp8],
            VideoCodecPreference::Av1 | VideoCodecPreference::Auto => {
                &[Self::Av1Nvenc, Self::H264Nvenc, Self::Vp8]
            }
            VideoCodecPreference::H264 => &[Self::H264Nvenc, Self::Vp8],
        };
        candidates
            .iter()
            .copied()
            .find(|codec| {
                codec
                    .required_elements()
                    .iter()
                    .all(|name| gst::ElementFactory::find(name).is_some())
            })
            .unwrap_or(Self::Vp8)
    }

    /// The display name reported in per-viewer stats.
    fn label(self) -> &'static str {
        match self {
            Self::H264Nvenc => "H264",
            Self::Av1Nvenc => "AV1",
            Self::Vp8 => "VP8",
        }
    }

    fn required_elements(self) -> &'static [&'static str] {
        match self {
            Self::H264Nvenc => &["nvh264enc", "h264parse", "rtph264pay"],
            Self::Av1Nvenc => &["nvav1enc", "av1parse", "rtpav1pay"],
            Self::Vp8 => &["vp8enc", "rtpvp8pay"],
        }
    }

    /// The raw pixel format the encoder consumes, produced once in the shared
    /// path so per-viewer branches convert nothing.
    fn raw_format(self) -> &'static str {
        match self {
            Self::H264Nvenc | Self::Av1Nvenc => "NV12",
            Self::Vp8 => "I420",
        }
    }

    fn uses_nvenc(self) -> bool {
        matches!(self, Self::H264Nvenc | Self::Av1Nvenc)
    }

    /// Applies an average target and a peak (VBR cap), translating to each
    /// encoder's bitrate unit: NVENC's `bitrate`/`max-bitrate` are kbit/sec,
    /// vp8enc's `target-bitrate` is bit/sec (CBR, so the peak is unused).
    fn set_rate(self, encoder: &gst::Element, target_kbps: u32, max_kbps: u32) {
        if self.uses_nvenc() {
            encoder.set_property("bitrate", target_kbps);
            set_max_bitrate(encoder, max_kbps.max(target_kbps));
        } else {
            encoder.set_property(
                "target-bitrate",
                i32::try_from(target_kbps.saturating_mul(1000)).unwrap_or(i32::MAX),
            );
        }
    }

    /// Builds the encode-to-RTP chain (encoder … capsfilter) and returns it
    /// with the encoder element for live bitrate control. The per-viewer branch
    /// links queue → valve into the first element and the last into webrtcbin.
    fn build_encode(self, initial_kbps: u32) -> Result<(Vec<gst::Element>, gst::Element), String> {
        let make = |name: &str| {
            gst::ElementFactory::make(name)
                .build()
                .map_err(|_| format!("`{name}` is unavailable"))
        };
        // NVENC low-latency VBR: a static screen costs almost nothing and a busy
        // screen bursts up to `max-bitrate`, so idle bandwidth stays low the way
        // a browser sender's does. The adaptive controller drives both the
        // average and the peak. Identical across codecs.
        let configure_nvenc = |encoder: &gst::Element| {
            encoder.set_property_from_str("rc-mode", "vbr");
            encoder.set_property_from_str("tune", "low-latency");
            encoder.set_property("zerolatency", true);
            encoder.set_property("bframes", 0u32);
            encoder.set_property("bitrate", initial_kbps);
            set_max_bitrate(encoder, initial_kbps);
        };
        let (encoder, middle, encoding_name): (gst::Element, Vec<gst::Element>, &str) = match self {
            Self::H264Nvenc => {
                let encoder = make("nvh264enc")?;
                configure_nvenc(&encoder);
                // Constrain to the profile every browser and the native decoder
                // accept; this drives the encoder's output before the parser.
                let profile = gst::ElementFactory::make("capsfilter")
                    .property(
                        "caps",
                        gst::Caps::builder("video/x-h264")
                            .field("profile", "constrained-baseline")
                            .build(),
                    )
                    .build()
                    .map_err(|_| "`capsfilter` is unavailable".to_owned())?;
                let parse = make("h264parse")?;
                parse.set_property("config-interval", -1i32);
                let pay = make("rtph264pay")?;
                pay.set_property("pt", 96u32);
                // Repeat SPS/PPS on every keyframe so a viewer that joins or
                // recovers mid-stream can start decoding without waiting.
                pay.set_property("config-interval", -1i32);
                pay.set_property_from_str("aggregate-mode", "zero-latency");
                (encoder, vec![profile, parse, pay], "H264")
            }
            Self::Av1Nvenc => {
                let encoder = make("nvav1enc")?;
                configure_nvenc(&encoder);
                let parse = make("av1parse")?;
                let pay = make("rtpav1pay")?;
                pay.set_property("pt", 96u32);
                (encoder, vec![parse, pay], "AV1")
            }
            Self::Vp8 => {
                let encoder = make("vp8enc")?;
                // deadline=1 selects realtime mode; PLI keyframes arrive as
                // upstream force-keyunit events.
                encoder.set_property("deadline", 1i64);
                encoder.set_property_from_str("end-usage", "cbr");
                encoder.set_property(
                    "target-bitrate",
                    i32::try_from(initial_kbps.saturating_mul(1000)).unwrap_or(i32::MAX),
                );
                encoder.set_property("cpu-used", 8i32);
                encoder.set_property("threads", 4i32);
                let pay = make("rtpvp8pay")?;
                pay.set_property("pt", 96u32);
                pay.set_property_from_str("picture-id-mode", "15-bit");
                (encoder, vec![pay], "VP8")
            }
        };
        let mut rtp_caps = gst::Caps::builder("application/x-rtp")
            .field("media", "video")
            .field("encoding-name", encoding_name)
            .field("clock-rate", 90_000)
            .field("payload", 96)
            .field("rtcp-fb-nack-pli", true);
        // The transport-wide congestion control extension carries the feedback
        // the GCC estimator consumes; without the local element the offer stays
        // honest and adaptation simply never engages.
        if gst::ElementFactory::find("rtphdrexttwcc").is_some() {
            rtp_caps = rtp_caps
                .field("rtcp-fb-transport-cc", true)
                .field("extmap-3", TWCC_EXTENSION_URI);
        }
        let caps = gst::ElementFactory::make("capsfilter")
            .property("caps", rtp_caps.build())
            .build()
            .map_err(|_| "`capsfilter` is unavailable".to_owned())?;
        let mut chain = vec![encoder.clone()];
        chain.extend(middle);
        chain.push(caps);
        Ok((chain, encoder))
    }
}

/// A locally generated test pattern with a burned-in timestamp; everything
/// downstream of the source is identical to a real capture.
#[derive(Debug, Clone, Copy)]
pub struct SyntheticSource {
    pub width: u32,
    pub height: u32,
    pub frame_rate: u32,
}

#[derive(Debug)]
pub enum SourceConfig {
    Synthetic(SyntheticSource),
    /// A negotiated screen or window capture; ends when the user stops the
    /// share from the system UI, which ends the broadcast.
    Screen(CaptureStream),
    /// The internal placeholder shown while nothing is shared: black frames
    /// at a token rate, encoding to near-zero bitrate. No capture grant
    /// exists while this source is active, so a broadcast can open before the
    /// presenter shares and stay alive between shares.
    Idle,
}

/// What sound accompanies the picture. Audio that cannot be captured
/// downgrades to a video-only broadcast rather than failing.
#[derive(Debug, Clone)]
pub enum AudioCapture {
    Disabled,
    /// Everything the presenter hears — the default output's monitor for a
    /// real capture, a soft test tone for the synthetic source.
    SystemMix,
    /// Capture and mix specific playback streams, each a PipeWire target
    /// object (object serial or node name). One target shares a single
    /// application; several express a system mix with some applications
    /// excluded — the caller assembles the set. Targets are a snapshot at
    /// start: streams appearing later are not added, and every listed stream
    /// must exist or the audio downgrades to video-only.
    Streams {
        targets: Vec<String>,
    },
}

pub struct BroadcastConfig {
    pub source: SourceConfig,
    pub audio: AudioCapture,
    pub video_codec: VideoCodecPreference,
    /// The maximum frame rate, in frames per second. Frames above this are
    /// dropped ahead of the tee; a slower source is passed through unchanged.
    pub frame_rate: u32,
    pub ice: IceConfiguration,
    pub force_relay: bool,
    /// A local self-preview of the captured screen, delivered as RGBA. `Some`
    /// taps the video tee into a leaky appsink so the presenter can see what
    /// they are sharing; a slow UI never backpressures the broadcast.
    pub preview_frames: Option<FrameSink>,
    /// The largest frame (width, height) fed to the encoders; a bigger
    /// capture is scaled down preserving aspect. `None` keeps the 2560x1440
    /// default, the web client's capture ceiling.
    pub capture_ceiling: Option<(u32, u32)>,
}

/// Per-viewer encoding policy. `bitrate_kbps` is the ceiling; with `adaptive`
/// set, transport feedback from this viewer steers the actual rate between a
/// safety floor and that ceiling, the way browsers adapt senders. Without it
/// the encoder holds the ceiling, subject only to
/// [`Broadcast::set_bitrate`].
#[derive(Debug, Clone, Copy)]
pub struct EncoderSettings {
    pub bitrate_kbps: u32,
    pub adaptive: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BroadcastEvent {
    /// An SDP offer to relay to this viewer, including renegotiations.
    Offer {
        peer_id: String,
        sdp: String,
        ice_restart: bool,
    },
    IceCandidate {
        peer_id: String,
        candidate: String,
        sdp_m_line_index: u32,
    },
    ViewerConnection {
        peer_id: String,
        state: ConnectionState,
    },
    ViewerStats {
        peer_id: String,
        stats: SenderStats,
    },
    /// A chat message arrived from a viewer over its data channel. The
    /// broadcast relays it to the other viewers itself; this reports it so the
    /// presenter can display it.
    Chat {
        peer_id: String,
        text: String,
    },
    /// The broadcast stopped and will not recover: the source or pipeline
    /// failed. Individual viewer failures do not end the broadcast.
    Ended {
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum BroadcastError {
    #[error("the media runtime could not be initialized: {0}")]
    Init(String),
    #[error(
        "the media component `{0}` is unavailable; install the GStreamer base, good, and bad plugin sets"
    )]
    MissingComponent(&'static str),
    #[error("the broadcast pipeline could not be started: {0}")]
    Start(String),
    #[error("the viewer connection could not be created: {0}")]
    Viewer(String),
    #[error("the viewer answer contained SDP the media stack could not parse")]
    InvalidSdp,
}

/// Sends one video source to any number of viewers, each on an independent
/// connection with its own encoder, so per-viewer quality never couples
/// viewers to each other.
///
/// The broadcast is the offerer: adding a viewer produces an `Offer` event,
/// and [`accept_answer`](Self::accept_answer) completes that negotiation.
/// Remote candidates are accepted in any order relative to the answer. Viewer
/// ids are caller-chosen; operations on unknown or already-removed viewers are
/// ignored, so admission races resolve harmlessly. [`pause`](Self::pause)
/// stops media flow to every viewer while keeping their connections
/// negotiated. The source can be swapped mid-broadcast
/// ([`replace_source`](Self::replace_source)) or parked entirely
/// ([`idle`](Self::idle)) without touching any viewer connection.
pub struct Broadcast {
    pipeline: gst::Pipeline,
    tee: gst::Element,
    /// Present when the broadcast carries audio.
    audio_tee: Option<gst::Element>,
    shared: Arc<Shared>,
    shutdown: Arc<AtomicBool>,
    bus_thread: Option<JoinHandle<()>>,
    /// The replaceable capture-side elements, from the source up to (not
    /// including) the fixed normalization tail.
    source_head: Mutex<Vec<gst::Element>>,
    /// First element of the fixed normalization tail (`videoconvert`); a
    /// replacement source head links into it.
    tail: gst::Element,
    /// The pinned-size capsfilter ending the tail, re-pinned from each
    /// source's caps notify.
    normalize: gst::Element,
    /// The configured capture ceiling `fit_within_capture_ceiling` scales
    /// oversized sources into, re-applied when the source is replaced.
    capture_ceiling: (i64, i64),
    /// The live capture backing the current source; dropping it revokes the
    /// compositor stream, so it is swapped together with the source head and
    /// released entirely while idle.
    capture: Mutex<Option<CaptureStream>>,
}

struct Shared {
    events: mpsc::UnboundedSender<BroadcastEvent>,
    viewers: Mutex<HashMap<String, ViewerEntry>>,
    /// Server-known display names by peer id. Chat relayed from a viewer is
    /// stamped with this name, never with the sender the payload claims.
    display_names: Mutex<HashMap<String, String>>,
    ice: Mutex<IceEndpoints>,
    video_codec: VideoCodec,
    force_relay: bool,
    paused: AtomicBool,
    /// Whether the idle placeholder is the current source. While idle the
    /// audio valves are held shut: nothing the presenter hears leaves the
    /// machine when they are not sharing.
    idle: AtomicBool,
    ended: AtomicBool,
}

struct ViewerEntry {
    webrtc: gst::Element,
    video_valve: gst::Element,
    audio_valve: Option<gst::Element>,
    encoder: gst::Element,
    branch: Vec<gst::Element>,
    tee_pad: gst::Pad,
    audio_tee_pad: Option<gst::Pad>,
    remote_description_set: bool,
    queued_candidates: Vec<(u32, String)>,
    stats_baseline: Option<StatsBaseline>,
    /// The video encoder's current target in kbps — the estimator's decision
    /// when adaptive, or the fixed ceiling. Written from the estimator
    /// callback and read by the stats reporter, so it is lock-free.
    target_kbps: Arc<AtomicU32>,
    /// The encoder's recently measured video output in kbps, written by the
    /// stats reporter and read by the adaptive controller to detect the
    /// application-limited region. Zero until the first stats sample.
    actual_send_kbps: Arc<AtomicU32>,
    codec: VideoCodec,
    /// The reliable data channel carrying chat with this viewer.
    chat: Option<gst_webrtc::WebRTCDataChannel>,
}

impl Broadcast {
    pub fn start(
        config: BroadcastConfig,
    ) -> Result<(Self, mpsc::UnboundedReceiver<BroadcastEvent>), BroadcastError> {
        crate::playback::ensure_gstreamer()
            .map_err(|error| BroadcastError::Init(error.to_string()))?;
        let source_element = match &config.source {
            SourceConfig::Synthetic(_) | SourceConfig::Idle => "videotestsrc",
            SourceConfig::Screen(_) => "pipewiresrc",
        };
        let video_codec = VideoCodec::select(config.video_codec);
        tracing::info!(codec = ?video_codec, "encoding video");
        for &element in [
            source_element,
            "tee",
            "webrtcbin",
            "valve",
            "videoconvert",
            "videoscale",
        ]
        .iter()
        .chain(video_codec.required_elements())
        {
            if gst::ElementFactory::find(element).is_none() {
                return Err(BroadcastError::MissingComponent(element));
            }
        }

        let (events, receiver) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            events,
            viewers: Mutex::new(HashMap::new()),
            display_names: Mutex::new(HashMap::new()),
            ice: Mutex::new(ice_endpoints(&config.ice)),
            video_codec,
            force_relay: config.force_relay,
            paused: AtomicBool::new(false),
            idle: AtomicBool::new(matches!(config.source, SourceConfig::Idle)),
            ended: AtomicBool::new(false),
        });

        let pipeline = gst::Pipeline::new();
        // The capture source offers the compositor's PipeWire clock, which the
        // pipeline would otherwise adopt — and in some sessions that clock
        // never advances, freezing every buffer timestamp at zero so all but
        // the first frame is discarded downstream as a duplicate. Frame
        // timestamps must come from the same clock the RTP stack paces by.
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        let start_error = |error: gst::glib::BoolError| BroadcastError::Start(error.to_string());
        let (head, capture) = build_source_head(config.source)?;
        // The source's frames are normalized once, ahead of the tee: converted
        // out of the capture buffer pool immediately (compositors stop
        // delivering when their small pool is held downstream), scaled to at
        // most the configured capture ceiling (1440p by default, the web
        // client's; raw 4K makes software encoding unsustainable) — and
        // reduced to the one format the encoders consume, so per-viewer
        // branches do no conversion work.
        let convert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(start_error)?;
        let scale = gst::ElementFactory::make("videoscale")
            .build()
            .map_err(start_error)?;
        // Cap the frame rate to the profile's target. `max-rate` drops frames
        // above the cap and never duplicates, so a static screen delivering a
        // couple of keepalive frames a second is passed through untouched while a
        // 60/120 Hz capture is thinned to the cap — the fps half of the
        // Text/Motion profile.
        let frame_rate = config.frame_rate.clamp(1, 120);
        let videorate = gst::ElementFactory::make("videorate")
            .property("max-rate", frame_rate as i32)
            .property("drop-only", true)
            .build()
            .map_err(start_error)?;
        // The target size is computed here, never left to caps fixation:
        // open-ended ranges make the scaler guess (it broke aspect on 4K and
        // overflowed on real compositor caps). The filter starts format-only
        // and is pinned to an exact size as soon as the source's actual pixel
        // dimensions are known — which also adapts if the captured source
        // changes size mid-stream.
        let raw_format = video_codec.raw_format();
        let normalize = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("format", raw_format)
                    .build(),
            )
            .build()
            .map_err(start_error)?;
        let capture_ceiling = config
            .capture_ceiling
            .map_or(DEFAULT_CAPTURE_CEILING, |(width, height)| {
                (i64::from(width.max(2)), i64::from(height.max(2)))
            });
        wire_caps_notify(&head, &normalize, raw_format, capture_ceiling)?;
        let tail = [convert, scale, videorate, normalize.clone()];
        // Zero viewers is a legal steady state: the tee must run unlinked
        // before the first admission and after the last departure.
        let tee = gst::ElementFactory::make("tee")
            .property("allow-not-linked", true)
            .build()
            .map_err(start_error)?;
        pipeline
            .add_many(&head)
            .map_err(|error| BroadcastError::Start(error.to_string()))?;
        pipeline
            .add_many(&tail)
            .map_err(|error| BroadcastError::Start(error.to_string()))?;
        pipeline
            .add(&tee)
            .map_err(|error| BroadcastError::Start(error.to_string()))?;
        gst::Element::link_many(&head).map_err(start_error)?;
        gst::Element::link_many(&tail).map_err(start_error)?;
        head.last()
            .expect("the source head is never empty")
            .link(&tail[0])
            .map_err(start_error)?;
        tail.last()
            .expect("the tail is never empty")
            .link(&tee)
            .map_err(start_error)?;

        // Presenter self-preview: a leaky branch off the tee that renders the
        // captured frames to RGBA for the local UI. Leaky so a slow preview
        // drops frames instead of stalling the viewers' encode branches.
        if let Some(frames) = &config.preview_frames {
            let queue = gst::ElementFactory::make("queue")
                .property_from_str("leaky", "downstream")
                .property("max-size-buffers", 2u32)
                .property("max-size-bytes", 0u32)
                .property("max-size-time", 0u64)
                .build()
                .map_err(start_error)?;
            let convert = gst::ElementFactory::make("videoconvert")
                .build()
                .map_err(start_error)?;
            let sink = frame_appsink(frames.clone()).map_err(BroadcastError::Start)?;
            let preview = [queue, convert, sink];
            pipeline
                .add_many(&preview)
                .map_err(|error| BroadcastError::Start(error.to_string()))?;
            gst::Element::link_many(&preview).map_err(start_error)?;
            let tee_pad = tee
                .request_pad_simple("src_%u")
                .ok_or_else(|| BroadcastError::Start("the preview tee pad is unavailable".into()))?;
            let queue_sink = preview[0]
                .static_pad("sink")
                .ok_or_else(|| BroadcastError::Start("the preview queue has no sink".into()))?;
            tee_pad
                .link(&queue_sink)
                .map_err(|_| BroadcastError::Start("the preview branch could not link".into()))?;
        }

        let audio_tee = match build_audio_chain(&pipeline, &config.audio, capture.is_some()) {
            Ok(tee) => tee,
            Err(reason) => {
                tracing::warn!(%reason, "sharing without audio");
                None
            }
        };

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|error| BroadcastError::Start(error.to_string()))?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let bus_thread = spawn_bus_thread(&pipeline, &shared, &shutdown)?;

        Ok((
            Self {
                pipeline,
                tee,
                audio_tee,
                shared,
                shutdown,
                bus_thread: Some(bus_thread),
                source_head: Mutex::new(head),
                tail: tail[0].clone(),
                normalize,
                capture_ceiling,
                capture: Mutex::new(capture),
            },
            receiver,
        ))
    }

    /// Swaps the capture head for a new source while every viewer connection,
    /// chat channel, and preview tap keeps running. The old head is parked
    /// under a blocking probe, dismantled, and its capture grant released;
    /// the caps-notify path renegotiates any resolution change downstream, so
    /// the per-viewer encoders and `webrtcbin` bins are never touched.
    pub fn replace_source(&self, source: SourceConfig) -> Result<(), BroadcastError> {
        let idle = matches!(source, SourceConfig::Idle);
        let (new_head, new_capture) = build_source_head(source)?;
        let raw_format = self.shared.video_codec.raw_format();
        {
            let mut head = self.source_head.lock().expect("source lock");
            let old_head = std::mem::take(&mut *head);
            if let Some(head_src) = old_head.last().and_then(|element| element.static_pad("src")) {
                // Park the old head's streaming thread before unlinking so it
                // can never push into a half-swapped tail. The probe fires
                // once the thread reaches the pad; a stalled source has
                // nothing in flight, which the timeout treats as parked.
                let (parked, wait_parked) = std::sync::mpsc::channel::<()>();
                head_src.add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, move |_, _| {
                    let _ = parked.send(());
                    gst::PadProbeReturn::Ok
                });
                let _ = wait_parked.recv_timeout(Duration::from_secs(1));
                if let Some(peer) = head_src.peer() {
                    let _ = head_src.unlink(&peer);
                }
            }
            // Null downstream-first: flushing the probe's own pad releases
            // the parked streaming thread (a FLUSHING return pauses a source
            // quietly), so the source can then stop its task without
            // deadlocking on the thread parked in the probe.
            for element in old_head.iter().rev() {
                let _ = element.set_state(gst::State::Null);
            }
            let _ = self.pipeline.remove_many(&old_head);

            let start_error = |error: gst::glib::BoolError| BroadcastError::Start(error.to_string());
            self.pipeline
                .add_many(&new_head)
                .map_err(|error| BroadcastError::Start(error.to_string()))?;
            gst::Element::link_many(&new_head).map_err(start_error)?;
            new_head
                .last()
                .expect("the source head is never empty")
                .link(&self.tail)
                .map_err(start_error)?;
            wire_caps_notify(&new_head, &self.normalize, raw_format, self.capture_ceiling)?;
            for element in new_head.iter().rev() {
                element
                    .sync_state_with_parent()
                    .map_err(|error| BroadcastError::Start(error.to_string()))?;
            }
            *head = new_head;
        }
        // The old grant is revoked here, after the pipeline stopped reading
        // from it; while idle no capture exists at all.
        *self.capture.lock().expect("capture lock") = new_capture;
        self.shared.idle.store(idle, Ordering::SeqCst);
        self.refresh_valves();
        Ok(())
    }

    /// Swaps the current source out for the internal idle placeholder and
    /// releases the screen capture, revoking the compositor's grant. Every
    /// peer connection, chat channel, and preview tap keeps running; going
    /// live again is [`replace_source`](Self::replace_source) with a real
    /// capture.
    pub fn idle(&self) -> Result<(), BroadcastError> {
        self.replace_source(SourceConfig::Idle)
    }

    /// Whether this broadcast carries audio; when it does not, viewers get a
    /// video-only stream.
    pub fn has_audio(&self) -> bool {
        self.audio_tee.is_some()
    }

    /// Builds this viewer's encoder and connection and starts negotiation; the
    /// resulting `Offer` event carries the viewer's id. Adding an id that is
    /// already active does nothing.
    pub fn add_viewer(
        &self,
        peer_id: &str,
        encoding: EncoderSettings,
    ) -> Result<(), BroadcastError> {
        let mut viewers = self.shared.viewers.lock().expect("viewer lock");
        if viewers.contains_key(peer_id) {
            return Ok(());
        }

        let build = |name: &'static str| {
            gst::ElementFactory::make(name)
                .build()
                .map_err(|_| BroadcastError::Viewer(format!("`{name}` is unavailable")))
        };
        // A shallow leaky queue decouples this viewer from the shared source
        // and drops stale frames instead of building latency when the encoder
        // falls behind. The buffer count is the only limit: the byte and time
        // defaults are smaller than single high-resolution frames and would
        // starve the flow entirely.
        let queue = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream")
            .property("max-size-buffers", 2u32)
            .property("max-size-bytes", 0u32)
            .property("max-size-time", 0u64)
            .build()
            .map_err(|_| BroadcastError::Viewer("`queue` is unavailable".into()))?;
        let valve = build("valve")?;
        valve.set_property("drop", self.shared.paused.load(Ordering::SeqCst));
        // Congestion control is delegated to GStreamer's GCC estimator
        // (rtpgccbwe), attached to the connection through webrtcbin's
        // aux-sender hook; without the element the viewer holds the ceiling.
        let adaptive_bwe = encoding.adaptive && gst::ElementFactory::find("rtpgccbwe").is_some();
        if encoding.adaptive && !adaptive_bwe {
            tracing::warn!("rtpgccbwe is unavailable; this viewer holds a fixed bitrate");
        }
        let initial_kbps = if adaptive_bwe {
            start_video_kbps(encoding.bitrate_kbps)
        } else {
            encoding.bitrate_kbps
        };
        let target_kbps = Arc::new(AtomicU32::new(initial_kbps));
        let actual_send_kbps = Arc::new(AtomicU32::new(0));
        let codec = self.shared.video_codec;
        let (encode_chain, encoder) = codec
            .build_encode(initial_kbps)
            .map_err(BroadcastError::Viewer)?;
        let caps = encode_chain
            .last()
            .cloned()
            .expect("the encode chain ends with a capsfilter");
        let webrtc = gst::ElementFactory::make("webrtcbin")
            .property_from_str("bundle-policy", "max-bundle")
            .build()
            .map_err(|_| BroadcastError::Viewer("`webrtcbin` is unavailable".into()))?;
        if self.shared.force_relay {
            webrtc.set_property_from_str("ice-transport-policy", "relay");
        }
        {
            let ice = self.shared.ice.lock().expect("ice lock");
            if let Some(stun) = &ice.stun_server {
                webrtc.set_property("stun-server", stun);
            }
            for turn in &ice.turn_servers {
                if !webrtc.emit_by_name::<bool>("add-turn-server", &[turn]) {
                    tracing::warn!("a TURN server from the room configuration was not accepted");
                }
            }
        }

        let mut branch = vec![queue.clone(), valve.clone()];
        branch.extend(encode_chain);
        branch.push(webrtc.clone());
        self.pipeline
            .add_many(&branch)
            .map_err(|error| BroadcastError::Viewer(error.to_string()))?;
        gst::Element::link_many(&branch[..branch.len() - 1])
            .map_err(|error| BroadcastError::Viewer(error.to_string()))?;
        let webrtc_sink = webrtc.request_pad_simple("sink_%u").ok_or_else(|| {
            BroadcastError::Viewer("the connection rejected a media stream".into())
        })?;
        caps.static_pad("src")
            .expect("capsfilter has a src pad")
            .link(&webrtc_sink)
            .map_err(|_| {
                BroadcastError::Viewer("the encoder could not reach the connection".into())
            })?;

        // The audio leg feeds the same connection as a second stream.
        let audio_leg_input = if self.audio_tee.is_some() {
            let audio_queue = gst::ElementFactory::make("queue")
                .property_from_str("leaky", "downstream")
                .property("max-size-buffers", 8u32)
                .property("max-size-bytes", 0u32)
                .property("max-size-time", 0u64)
                .build()
                .map_err(|_| BroadcastError::Viewer("`queue` is unavailable".into()))?;
            let audio_valve = build("valve")?;
            audio_valve.set_property(
                "drop",
                self.shared.paused.load(Ordering::SeqCst)
                    || self.shared.idle.load(Ordering::SeqCst),
            );
            let audio_encoder = build("opusenc")?;
            audio_encoder.set_property("bitrate", 128_000_i32);
            // In-band FEC carries recovery data for the previous packet, but
            // only sizes that redundancy when told how much loss to expect —
            // left at 0 it does nothing. Sized for the loss a congested shared
            // uplink produces (audio competes with video on the same
            // transport); Opus keeps the overhead modest on clean paths.
            audio_encoder.set_property("inband-fec", true);
            audio_encoder.set_property("packet-loss-percentage", 20_i32);
            let audio_payloader = build("rtpopuspay")?;
            audio_payloader.set_property("pt", 111u32);
            let mut audio_rtp_caps = gst::Caps::builder("application/x-rtp")
                .field("media", "audio")
                .field("encoding-name", "OPUS")
                .field("clock-rate", 48_000)
                .field("payload", 111);
            if gst::ElementFactory::find("rtphdrexttwcc").is_some() {
                audio_rtp_caps = audio_rtp_caps
                    .field("rtcp-fb-transport-cc", true)
                    .field("extmap-3", TWCC_EXTENSION_URI);
            }
            let audio_caps = gst::ElementFactory::make("capsfilter")
                .property("caps", audio_rtp_caps.build())
                .build()
                .map_err(|_| BroadcastError::Viewer("`capsfilter` is unavailable".into()))?;
            let audio_leg = [
                audio_queue.clone(),
                audio_valve.clone(),
                audio_encoder,
                audio_payloader,
                audio_caps.clone(),
            ];
            self.pipeline
                .add_many(&audio_leg)
                .map_err(|error| BroadcastError::Viewer(error.to_string()))?;
            gst::Element::link_many(&audio_leg)
                .map_err(|error| BroadcastError::Viewer(error.to_string()))?;
            let audio_sink = webrtc.request_pad_simple("sink_%u").ok_or_else(|| {
                BroadcastError::Viewer("the connection rejected an audio stream".into())
            })?;
            audio_caps
                .static_pad("src")
                .expect("capsfilter has a src pad")
                .link(&audio_sink)
                .map_err(|_| {
                    BroadcastError::Viewer(
                        "the audio encoder could not reach the connection".into(),
                    )
                })?;
            branch.extend(audio_leg);
            Some((audio_queue, audio_valve))
        } else {
            None
        };
        let (audio_queue, audio_valve) = match audio_leg_input {
            Some((queue, valve)) => (Some(queue), Some(valve)),
            None => (None, None),
        };

        // Create the chat data channel before negotiation so it rides the
        // initial offer rather than forcing a renegotiation. The bin must be at
        // least READY for `create-data-channel` to succeed.
        let _ = webrtc.set_state(gst::State::Ready);
        let chat = create_chat_channel(&webrtc, &self.shared, peer_id);
        self.wire_viewer_signals(peer_id, &webrtc);
        if adaptive_bwe {
            wire_gcc_bwe(
                &webrtc,
                &encoder,
                codec,
                encoding.bitrate_kbps,
                self.audio_tee.is_some(),
                Arc::clone(&target_kbps),
                Arc::clone(&actual_send_kbps),
            );
        }
        for index in 0..=i32::from(audio_queue.is_some()) {
            if let Some(transceiver) = webrtc
                .emit_by_name::<Option<gst_webrtc::WebRTCRTPTransceiver>>(
                    "get-transceiver",
                    &[&index],
                )
            {
                transceiver.set_property(
                    "direction",
                    gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly,
                );
                if index == 0 {
                    transceiver.set_property("do-nack", true);
                }
            }
        }

        for element in branch.iter().rev() {
            element
                .sync_state_with_parent()
                .map_err(|error| BroadcastError::Viewer(error.to_string()))?;
        }
        let tee_pad = self.tee.request_pad_simple("src_%u").ok_or_else(|| {
            BroadcastError::Viewer("the source could not add another viewer".into())
        })?;
        tee_pad
            .link(&queue.static_pad("sink").expect("queue has a sink pad"))
            .map_err(|_| BroadcastError::Viewer("the source could not reach the encoder".into()))?;
        let audio_tee_pad = match (&self.audio_tee, audio_queue) {
            (Some(audio_tee), Some(audio_queue)) => {
                let pad = audio_tee.request_pad_simple("src_%u").ok_or_else(|| {
                    BroadcastError::Viewer("the audio source could not add another viewer".into())
                })?;
                pad.link(
                    &audio_queue
                        .static_pad("sink")
                        .expect("queue has a sink pad"),
                )
                .map_err(|_| {
                    BroadcastError::Viewer("the audio source could not reach the encoder".into())
                })?;
                Some(pad)
            }
            _ => None,
        };

        viewers.insert(
            peer_id.to_owned(),
            ViewerEntry {
                webrtc,
                video_valve: valve,
                audio_valve,
                encoder,
                branch,
                tee_pad,
                audio_tee_pad,
                remote_description_set: false,
                queued_candidates: Vec::new(),
                stats_baseline: None,
                target_kbps,
                actual_send_kbps,
                codec,
                chat,
            },
        );
        Ok(())
    }

    /// Sends a chat message to every viewer over their data channels. Messages
    /// to a viewer whose channel is not yet open are dropped.
    pub fn send_chat(&self, text: &str) {
        let viewers = self.shared.viewers.lock().expect("viewer lock");
        for entry in viewers.values() {
            send_chat_string(entry.chat.as_ref(), text);
        }
    }

    /// Records the server-known display name chat from this viewer is stamped
    /// with when relayed; `None` falls back to "Viewer".
    pub fn set_viewer_display_name(&self, peer_id: &str, display_name: Option<&str>) {
        let mut names = self.shared.display_names.lock().expect("display name lock");
        match display_name {
            Some(name) => {
                names.insert(peer_id.to_owned(), name.to_owned());
            }
            None => {
                names.remove(peer_id);
            }
        }
    }

    /// Disconnects a viewer and dismantles their encoder branch. The shared
    /// source and all other viewers are untouched. The dismantling itself
    /// happens on a background thread — the bounded ICE-gathering wait it
    /// needs must never stall the caller — so the branch may outlive this
    /// call by a few seconds.
    pub fn remove_viewer(&self, peer_id: &str) {
        self.shared
            .display_names
            .lock()
            .expect("display name lock")
            .remove(peer_id);
        let Some(entry) = self
            .shared
            .viewers
            .lock()
            .expect("viewer lock")
            .remove(peer_id)
        else {
            return;
        };
        let pipeline = self.pipeline.clone();
        let tee = self.tee.clone();
        let audio_tee = self.audio_tee.clone();
        crate::teardown::spawn_teardown("clarity-media-viewer-teardown", move || {
            settle_ice_gathering(&entry.webrtc, Instant::now() + Duration::from_secs(5));
            dismantle_viewer(&pipeline, &tee, audio_tee.as_ref(), &entry);
        });
    }

    /// Applies a viewer's SDP answer; their queued candidates are flushed once
    /// it takes effect.
    pub fn accept_answer(&self, peer_id: &str, sdp: &str) -> Result<(), BroadcastError> {
        let message = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes())
            .map_err(|_| BroadcastError::InvalidSdp)?;
        let answer =
            gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Answer, message);
        let viewers = self.shared.viewers.lock().expect("viewer lock");
        let Some(entry) = viewers.get(peer_id) else {
            return Ok(());
        };
        let webrtc = entry.webrtc.clone();
        drop(viewers);
        let shared = Arc::clone(&self.shared);
        let peer = peer_id.to_owned();
        let applied = gst::Promise::with_change_func(move |reply| {
            if reply.is_err() {
                tracing::warn!("a viewer answer could not be applied");
                return;
            }
            let mut viewers = shared.viewers.lock().expect("viewer lock");
            if let Some(entry) = viewers.get_mut(&peer) {
                entry.remote_description_set = true;
                for (mline, candidate) in entry.queued_candidates.drain(..) {
                    entry
                        .webrtc
                        .emit_by_name::<()>("add-ice-candidate", &[&mline, &candidate]);
                }
            }
        });
        webrtc.emit_by_name::<()>("set-remote-description", &[&answer, &applied]);
        Ok(())
    }

    /// Accepts a viewer's ICE candidate, in any order relative to the answer.
    pub fn add_remote_candidate(&self, peer_id: &str, sdp_m_line_index: u32, candidate: &str) {
        if candidate.trim().is_empty() {
            return;
        }
        let mut viewers = self.shared.viewers.lock().expect("viewer lock");
        let Some(entry) = viewers.get_mut(peer_id) else {
            return;
        };
        if entry.remote_description_set {
            entry
                .webrtc
                .emit_by_name::<()>("add-ice-candidate", &[&sdp_m_line_index, &candidate]);
        } else {
            entry
                .queued_candidates
                .push((sdp_m_line_index, candidate.to_owned()));
        }
    }

    /// Renegotiates this viewer's transport, producing a fresh `Offer` event
    /// with `ice_restart` set. Media to other viewers is unaffected.
    pub fn restart_ice(&self, peer_id: &str) {
        let viewers = self.shared.viewers.lock().expect("viewer lock");
        let Some(entry) = viewers.get(peer_id) else {
            return;
        };
        let webrtc = entry.webrtc.clone();
        drop(viewers);
        negotiate(&webrtc, &self.shared, peer_id, true);
    }

    /// Overrides this viewer's video bitrate immediately, without
    /// renegotiation. On an adaptive viewer the estimator's next update takes
    /// over again; this is primarily for fixed-bitrate viewers.
    pub fn set_bitrate(&self, peer_id: &str, bitrate_kbps: u32) {
        let viewers = self.shared.viewers.lock().expect("viewer lock");
        if let Some(entry) = viewers.get(peer_id) {
            entry.target_kbps.store(bitrate_kbps, Ordering::Relaxed);
            entry.codec.set_rate(&entry.encoder, bitrate_kbps, bitrate_kbps);
        }
    }

    /// Stops media flow to every viewer while keeping their connections
    /// negotiated; [`resume`](Self::resume) restarts flow without any
    /// renegotiation. Viewers added while paused start paused.
    pub fn pause(&self) {
        self.set_paused(true);
    }

    pub fn resume(&self) {
        self.set_paused(false);
    }

    pub fn close(mut self) {
        self.shutdown_internal();
    }

    fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::SeqCst);
        self.refresh_valves();
    }

    /// Applies the pause and idle switches to every viewer's valves. Video
    /// flows unless paused (the idle placeholder is itself video); audio is
    /// also held while idle, so nothing the presenter hears leaves the
    /// machine when they are not sharing.
    fn refresh_valves(&self) {
        let paused = self.shared.paused.load(Ordering::SeqCst);
        let idle = self.shared.idle.load(Ordering::SeqCst);
        let viewers = self.shared.viewers.lock().expect("viewer lock");
        for entry in viewers.values() {
            entry.video_valve.set_property("drop", paused);
            if let Some(audio_valve) = &entry.audio_valve {
                audio_valve.set_property("drop", paused || idle);
            }
        }
    }

    fn wire_viewer_signals(&self, peer_id: &str, webrtc: &gst::Element) {
        {
            let shared = Arc::clone(&self.shared);
            let peer = peer_id.to_owned();
            let webrtc_clone = webrtc.clone();
            webrtc.connect("on-negotiation-needed", false, move |_| {
                negotiate(&webrtc_clone, &shared, &peer, false);
                None
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            let peer = peer_id.to_owned();
            webrtc.connect("on-ice-candidate", false, move |values| {
                let Ok(mline) = values[1].get::<u32>() else {
                    return None;
                };
                let Ok(candidate) = values[2].get::<String>() else {
                    return None;
                };
                if !candidate.trim().is_empty() {
                    let _ = shared.events.send(BroadcastEvent::IceCandidate {
                        peer_id: peer.clone(),
                        candidate,
                        sdp_m_line_index: mline,
                    });
                }
                None
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            let peer = peer_id.to_owned();
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
                let _ = shared.events.send(BroadcastEvent::ViewerConnection {
                    peer_id: peer.clone(),
                    state,
                });
            });
        }
    }

    fn shutdown_internal(&mut self) {
        // Repeated shutdown (close followed by drop) is a no-op.
        let Some(bus_thread) = self.bus_thread.take() else {
            return;
        };
        let webrtcs: Vec<gst::Element> = {
            let viewers = self.shared.viewers.lock().expect("viewer lock");
            viewers.values().map(|entry| entry.webrtc.clone()).collect()
        };
        let capture = self.capture.lock().expect("capture lock").take();
        let pipeline = self.pipeline.clone();
        let shutdown = Arc::clone(&self.shutdown);
        // The gathering settle and the state teardown can stall for seconds;
        // they run on a background thread so close() and drop return
        // promptly (the caller is typically the GUI thread).
        crate::teardown::spawn_teardown("clarity-media-broadcast-teardown", move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            for webrtc in &webrtcs {
                settle_ice_gathering(webrtc, deadline);
            }
            shutdown.store(true, Ordering::SeqCst);
            let _ = pipeline.set_state(gst::State::Null);
            let _ = bus_thread.join();
            // The capture grant outlives the pipeline that reads from it
            // and is revoked only once the pipeline has stopped.
            drop(capture);
        });
    }
}

impl Drop for Broadcast {
    fn drop(&mut self) {
        self.shutdown_internal();
    }
}

/// Replaces the ICE server set used for viewers added from now on; existing
/// connections keep the servers they negotiated with. Room TURN credentials
/// are short-lived, so refreshed configurations matter for late joiners.
impl Broadcast {
    pub fn set_ice(&self, configuration: &IceConfiguration) {
        *self.shared.ice.lock().expect("ice lock") = ice_endpoints(configuration);
    }
}

/// The reliable data channel label shared with the web client
/// (`web/src/lib/chat/chat-channel.ts`, `CHAT_CHANNEL_LABEL`). Both sides must
/// use this exact label and the `ChatMessage` JSON envelope so web and native
/// peers interoperate in the same room.
pub(crate) const CHAT_CHANNEL_LABEL: &str = "chat";

/// Creates the presenter's side of the reliable chat channel for one viewer and
/// wires incoming messages to relay-and-report. `None` if the platform stack
/// refuses the channel; chat simply stays unavailable for that viewer.
fn create_chat_channel(
    webrtc: &gst::Element,
    shared: &Arc<Shared>,
    peer_id: &str,
) -> Option<gst_webrtc::WebRTCDataChannel> {
    let channel = webrtc.emit_by_name::<Option<gst_webrtc::WebRTCDataChannel>>(
        "create-data-channel",
        &[&CHAT_CHANNEL_LABEL, &None::<gst::Structure>],
    )?;
    let shared = Arc::clone(shared);
    let sender = peer_id.to_owned();
    channel.connect_on_message_string(move |_channel, message| {
        if let Some(text) = message {
            relay_chat(&shared, &sender, text);
        }
    });
    Some(channel)
}

/// Fans a viewer's message out to every *other* viewer and reports it to the
/// presenter, so one broadcast acts as the chat hub without the server.
///
/// The envelope's `sender` field is client-asserted, so the hub replaces it
/// with the server-known display name of the peer the payload arrived from; a
/// viewer cannot speak as the presenter or another viewer. A payload that is
/// not a `ChatMessage` envelope is dropped, matching the web hub.
fn relay_chat(shared: &Arc<Shared>, from: &str, text: &str) {
    let Ok(message) = serde_json::from_str::<ChatMessage>(text) else {
        tracing::warn!(viewer = %from, "dropping a chat payload that is not a ChatMessage envelope");
        return;
    };
    let sender = shared
        .display_names
        .lock()
        .expect("display name lock")
        .get(from)
        .cloned()
        .unwrap_or_else(|| "Viewer".to_owned());
    let stamped = serde_json::to_string(&ChatMessage {
        sender,
        text: message.text,
    })
    .expect("chat messages always serialize");
    {
        let viewers = shared.viewers.lock().expect("viewer lock");
        for (peer, entry) in viewers.iter() {
            if peer != from {
                send_chat_string(entry.chat.as_ref(), &stamped);
            }
        }
    }
    let _ = shared.events.send(BroadcastEvent::Chat {
        peer_id: from.to_owned(),
        text: stamped,
    });
}

/// Sends `text` on a data channel when it is open, ignoring it otherwise.
fn send_chat_string(channel: Option<&gst_webrtc::WebRTCDataChannel>, text: &str) {
    if let Some(channel) = channel
        && channel.ready_state() == gst_webrtc::WebRTCDataChannelState::Open
    {
        channel.send_string(Some(text));
    }
}

fn negotiate(webrtc: &gst::Element, shared: &Arc<Shared>, peer_id: &str, ice_restart: bool) {
    if shared.ended.load(Ordering::SeqCst) {
        return;
    }
    let options = ice_restart.then(|| {
        gst::Structure::builder("options")
            .field("ice-restart", true)
            .build()
    });
    let events = shared.events.clone();
    let peer = peer_id.to_owned();
    let webrtc_clone = webrtc.clone();
    let promise = gst::Promise::with_change_func(move |reply| {
        let offer = match reply {
            Ok(Some(structure)) => structure
                .get::<gst_webrtc::WebRTCSessionDescription>("offer")
                .ok(),
            _ => None,
        };
        let Some(offer) = offer else {
            tracing::warn!("an offer could not be created for a viewer");
            return;
        };
        webrtc_clone.emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);
        if let Ok(sdp) = offer.sdp().as_text() {
            let _ = events.send(BroadcastEvent::Offer {
                peer_id: peer.clone(),
                sdp,
                ice_restart,
            });
        }
    });
    webrtc.emit_by_name::<()>("create-offer", &[&options, &promise]);
}

/// Waits for ICE gathering to settle before teardown; dismantling a
/// connection mid-gather races inside the platform WebRTC stack and corrupts
/// the heap. Always called from a background thread, never the caller's.
fn settle_ice_gathering(webrtc: &gst::Element, deadline: Instant) {
    while webrtc.property::<gst_webrtc::WebRTCICEGatheringState>("ice-gathering-state")
        == gst_webrtc::WebRTCICEGatheringState::Gathering
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Unlinks a removed viewer's tee pads, each under its own idle probe so
/// neither streaming thread ever pushes into a half-removed branch; the last
/// probe to run dismantles the elements.
fn dismantle_viewer(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    audio_tee: Option<&gst::Element>,
    entry: &ViewerEntry,
) {
    let pipeline = pipeline.clone();
    let tee = tee.clone();
    let audio_tee = audio_tee.cloned();
    let branch = entry.branch.clone();
    let audio_tee_pad = entry.audio_tee_pad.clone();
    entry
        .tee_pad
        .add_probe(gst::PadProbeType::IDLE, move |pad, _| {
            if let Some(peer) = pad.peer() {
                let _ = pad.unlink(&peer);
            }
            tee.release_request_pad(pad);
            match (&audio_tee, &audio_tee_pad) {
                (Some(audio_tee), Some(audio_pad)) => {
                    let pipeline = pipeline.clone();
                    let audio_tee = audio_tee.clone();
                    let branch = branch.clone();
                    audio_pad.add_probe(gst::PadProbeType::IDLE, move |pad, _| {
                        if let Some(peer) = pad.peer() {
                            let _ = pad.unlink(&peer);
                        }
                        audio_tee.release_request_pad(pad);
                        dismantle_branch(&pipeline, &branch);
                        gst::PadProbeReturn::Remove
                    });
                }
                _ => dismantle_branch(&pipeline, &branch),
            }
            gst::PadProbeReturn::Remove
        });
}

/// Builds the replaceable capture-side element chain for one source and
/// returns it with the capture grant that backs it, if any. The elements are
/// not yet part of a pipeline.
fn build_source_head(
    source: SourceConfig,
) -> Result<(Vec<gst::Element>, Option<CaptureStream>), BroadcastError> {
    let start_error = |error: gst::glib::BoolError| BroadcastError::Start(error.to_string());
    match source {
        SourceConfig::Synthetic(synthetic) => {
            let source = gst::ElementFactory::make("videotestsrc")
                .property("is-live", true)
                .property_from_str("pattern", "smpte")
                .build()
                .map_err(start_error)?;
            let overlay = gst::ElementFactory::make("timeoverlay")
                .build()
                .map_err(start_error)?;
            let capsfilter = gst::ElementFactory::make("capsfilter")
                .property(
                    "caps",
                    gst::Caps::builder("video/x-raw")
                        .field("width", synthetic.width.max(2) as i32)
                        .field("height", synthetic.height.max(2) as i32)
                        .field(
                            "framerate",
                            gst::Fraction::new(synthetic.frame_rate.max(1) as i32, 1),
                        )
                        .build(),
                )
                .build()
                .map_err(start_error)?;
            Ok((vec![source, overlay, capsfilter], None))
        }
        SourceConfig::Idle => {
            let source = gst::ElementFactory::make("videotestsrc")
                .property("is-live", true)
                .property_from_str("pattern", "black")
                .build()
                .map_err(start_error)?;
            let capsfilter = gst::ElementFactory::make("capsfilter")
                .property(
                    "caps",
                    gst::Caps::builder("video/x-raw")
                        .field("width", IDLE_WIDTH)
                        .field("height", IDLE_HEIGHT)
                        .field("framerate", gst::Fraction::new(IDLE_FRAME_RATE, 1))
                        .build(),
                )
                .build()
                .map_err(start_error)?;
            Ok((vec![source, capsfilter], None))
        }
        SourceConfig::Screen(stream) => {
            use std::os::fd::AsRawFd;
            let source = gst::ElementFactory::make("pipewiresrc")
                .property("fd", stream.fd.as_raw_fd())
                .property("path", stream.node_id.to_string())
                .property("do-timestamp", true)
                // Compositors deliver frames only when the captured
                // content changes; a static screen would otherwise send
                // one frame and starve the encoder, the RTP flow, and the
                // transport feedback that adaptation runs on. Re-sending
                // the last frame keeps everything alive, and an unchanged
                // frame encodes to almost nothing.
                .property("keepalive-time", 500_i32)
                .build()
                .map_err(start_error)?;
            // System-memory frames only: encoder branches consume raw
            // video directly, and DMA-BUF import would require the GL
            // stack this software path does not carry.
            let capsfilter = gst::ElementFactory::make("capsfilter")
                .property("caps", gst::Caps::new_empty_simple("video/x-raw"))
                .build()
                .map_err(start_error)?;
            Ok((vec![source, capsfilter], Some(stream)))
        }
    }
}

/// Pins the normalization capsfilter to the source's actual pixel dimensions
/// as soon as — and whenever — they are known, so caps fixation never guesses
/// a size and a mid-stream resolution change re-pins automatically.
fn wire_caps_notify(
    head: &[gst::Element],
    normalize: &gst::Element,
    raw_format: &'static str,
    capture_ceiling: (i64, i64),
) -> Result<(), BroadcastError> {
    let source_pad = head
        .first()
        .and_then(|source| source.static_pad("src"))
        .ok_or_else(|| BroadcastError::Start("the source has no output".into()))?;
    let normalize = normalize.clone();
    source_pad.connect_notify(Some("caps"), move |pad, _| {
        let Some(caps) = pad.current_caps() else {
            return;
        };
        let Some(video) = caps.structure(0) else {
            return;
        };
        let (Ok(width), Ok(height)) = (video.get::<i32>("width"), video.get::<i32>("height"))
        else {
            return;
        };
        let (target_width, target_height) =
            fit_within_capture_ceiling(width, height, capture_ceiling);
        normalize.set_property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("format", raw_format)
                .field("width", target_width)
                .field("height", target_height)
                .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                .build(),
        );
    });
    Ok(())
}

fn spawn_bus_thread(
    pipeline: &gst::Pipeline,
    shared: &Arc<Shared>,
    shutdown: &Arc<AtomicBool>,
) -> Result<JoinHandle<()>, BroadcastError> {
    let bus = pipeline
        .bus()
        .ok_or_else(|| BroadcastError::Start("the pipeline has no message bus".into()))?;
    let shared = Arc::clone(shared);
    let shutdown = Arc::clone(shutdown);
    std::thread::Builder::new()
        .name("clarity-media-broadcast-bus".into())
        .spawn(move || {
            let mut last_stats = Instant::now();
            while !shutdown.load(Ordering::SeqCst) {
                if let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(250)) {
                    use gst::MessageView;
                    match message.view() {
                        MessageView::Error(error) => {
                            end(
                                &shared,
                                &format!(
                                    "broadcast pipeline error: {}{}",
                                    error.error(),
                                    error
                                        .debug()
                                        .map(|debug| format!(" ({debug})"))
                                        .unwrap_or_default()
                                ),
                            );
                            return;
                        }
                        MessageView::Eos(_) => {
                            end(&shared, "the source ended");
                            return;
                        }
                        _ => {}
                    }
                }
                if last_stats.elapsed() >= STATS_INTERVAL {
                    last_stats = Instant::now();
                    request_all_stats(&shared);
                }
            }
        })
        .map_err(|error| BroadcastError::Start(error.to_string()))
}

fn request_all_stats(shared: &Arc<Shared>) {
    let peers: Vec<(String, gst::Element)> = {
        let viewers = shared.viewers.lock().expect("viewer lock");
        viewers
            .iter()
            .map(|(peer_id, entry)| (peer_id.clone(), entry.webrtc.clone()))
            .collect()
    };
    for (peer_id, webrtc) in peers {
        let shared = Arc::clone(shared);
        let promise = gst::Promise::with_change_func(move |reply| {
            let Ok(Some(report)) = reply else {
                return;
            };
            let parsed = stats::parse_sender_report(report);
            let (bitrate, target_kbps, codec) = {
                let mut viewers = shared.viewers.lock().expect("viewer lock");
                let Some(entry) = viewers.get_mut(&peer_id) else {
                    return;
                };
                let bitrate = parsed.bytes_sent.and_then(|bytes| {
                    let bitrate = stats::bitrate_kbps(entry.stats_baseline.as_ref(), bytes);
                    entry.stats_baseline = Some(StatsBaseline {
                        at: Instant::now(),
                        bytes_received: bytes,
                    });
                    bitrate
                });
                // Feed the measured output to the adaptive controller so it can
                // tell an idle screen from a congested link.
                if let Some(kbps) = bitrate {
                    entry.actual_send_kbps.store(kbps, Ordering::Relaxed);
                }
                (
                    bitrate,
                    entry.target_kbps.load(Ordering::Relaxed),
                    entry.codec.label(),
                )
            };
            let _ = shared.events.send(BroadcastEvent::ViewerStats {
                peer_id: peer_id.clone(),
                stats: SenderStats {
                    bitrate_kbps: bitrate,
                    packets_lost: parsed.packets_lost,
                    packets_sent: parsed.packets_sent,
                    round_trip_ms: parsed.round_trip_ms,
                    target_kbps,
                    codec: Some(codec.to_owned()),
                },
            });
        });
        webrtc.emit_by_name::<()>("get-stats", &[&None::<gst::Pad>, &promise]);
    }
}

/// Attaches GStreamer's GCC bandwidth estimator to this viewer's connection
/// through webrtcbin's aux-sender hook and feeds its estimate through the
/// application-limited-region controller. The transport-wide estimate has the
/// fixed audio budget subtracted so congestion throttles video and never audio;
/// the controller then holds the rate steady on an idle screen (where the raw
/// estimate collapses) instead of chasing it down. See [`crate::rate`].
fn wire_gcc_bwe(
    webrtc: &gst::Element,
    encoder: &gst::Element,
    codec: VideoCodec,
    ceiling_kbps: u32,
    has_audio: bool,
    target_kbps: Arc<AtomicU32>,
    actual_send_kbps: Arc<AtomicU32>,
) {
    let audio_bps = if has_audio { AUDIO_BITRATE_BPS } else { 0 };
    let audio_kbps = audio_bps / 1000;
    let min_bps = audio_bps + VIDEO_MIN_KBPS * 1000;
    let max_bps = audio_bps + ceiling_kbps.saturating_mul(1000);
    let start_bps = audio_bps + start_video_kbps(ceiling_kbps) * 1000;
    let encoder = encoder.clone();
    webrtc.connect("request-aux-sender", false, move |_| {
        let Ok(bwe) = gst::ElementFactory::make("rtpgccbwe").build() else {
            return None;
        };
        // Bounds and the starting estimate are only settable before the
        // element is started, which is why they are configured here.
        bwe.set_property("min-bitrate", min_bps);
        bwe.set_property("max-bitrate", max_bps);
        bwe.set_property("estimated-bitrate", start_bps);
        let encoder = encoder.clone();
        let target_kbps = Arc::clone(&target_kbps);
        let actual_send_kbps = Arc::clone(&actual_send_kbps);
        let controller = std::sync::Mutex::new(AdaptiveController::new(
            VIDEO_MIN_KBPS,
            ceiling_kbps,
            start_video_kbps(ceiling_kbps),
        ));
        bwe.connect_notify(Some("estimated-bitrate"), move |bwe, _| {
            let estimate = bwe.property::<u32>("estimated-bitrate");
            let estimate_kbps = estimate.saturating_sub(audio_bps).max(VIDEO_MIN_KBPS * 1000) / 1000;
            // The stats reporter measures the whole transport; the video output
            // is that minus the fixed audio budget.
            let actual_kbps = actual_send_kbps
                .load(Ordering::Relaxed)
                .saturating_sub(audio_kbps);
            let command = controller
                .lock()
                .expect("rate lock")
                .on_estimate(estimate_kbps, actual_kbps);
            codec.set_rate(&encoder, command.target_kbps, command.max_kbps);
            target_kbps.store(command.target_kbps, Ordering::Relaxed);
            tracing::debug!(
                estimate_kbps,
                actual_kbps,
                target_kbps = command.target_kbps,
                "gcc estimate"
            );
        });
        Some(bwe.to_value())
    });
}

fn end(shared: &Shared, reason: &str) {
    if !shared.ended.swap(true, Ordering::SeqCst) {
        let _ = shared.events.send(BroadcastEvent::Ended {
            reason: reason.to_owned(),
        });
    }
}

fn dismantle_branch(pipeline: &gst::Pipeline, branch: &[gst::Element]) {
    for element in branch {
        let _ = element.set_state(gst::State::Null);
    }
    let _ = pipeline.remove_many(branch);
}

/// Builds the shared audio path up to its tee: what the presenter hears (the
/// default output's monitor) for a real capture, a quiet tone for the
/// synthetic source. Normalized to the one format the Opus encoders consume
/// so per-viewer branches do no conversion work.
fn build_audio_chain(
    pipeline: &gst::Pipeline,
    audio: &AudioCapture,
    live_capture: bool,
) -> Result<Option<gst::Element>, String> {
    if matches!(audio, AudioCapture::Disabled) {
        return Ok(None);
    }
    if let AudioCapture::Streams { targets } = audio
        && targets.is_empty()
    {
        return Err("no application audio streams were available to capture".to_owned());
    }
    for element in ["opusenc", "rtpopuspay"] {
        if gst::ElementFactory::find(element).is_none() {
            return Err(format!("the audio component `{element}` is unavailable"));
        }
    }
    // A head element whose src pad carries the captured audio: a single source
    // directly, or an audiomixer summing several tapped streams.
    let head = build_audio_head(pipeline, audio, live_capture)?;
    let convert = make_audio("audioconvert")?;
    let resample = make_audio("audioresample")?;
    let normalize = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("audio/x-raw")
                .field("rate", 48_000)
                .field("channels", 2)
                .build(),
        )
        .build()
        .map_err(|error| error.to_string())?;
    let tee = gst::ElementFactory::make("tee")
        .property("allow-not-linked", true)
        .build()
        .map_err(|error| error.to_string())?;
    let tail = [convert, resample, normalize, tee.clone()];
    pipeline
        .add_many(&tail)
        .map_err(|error| error.to_string())?;
    gst::Element::link_many(&tail).map_err(|error| error.to_string())?;
    head.link(&tail[0]).map_err(|error| error.to_string())?;
    Ok(Some(tee))
}

fn make_audio(name: &'static str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| format!("the audio component `{name}` is unavailable"))
}

fn build_audio_head(
    pipeline: &gst::Pipeline,
    audio: &AudioCapture,
    live_capture: bool,
) -> Result<gst::Element, String> {
    match audio {
        AudioCapture::Disabled => unreachable!("handled by the caller"),
        AudioCapture::SystemMix if live_capture => {
            // "@DEFAULT_MONITOR@" resolves server-side to the monitor of the
            // current default output, following device switches.
            let source = gst::ElementFactory::make("pulsesrc")
                .property("device", "@DEFAULT_MONITOR@")
                .build()
                .map_err(|error| error.to_string())?;
            pipeline.add(&source).map_err(|error| error.to_string())?;
            Ok(source)
        }
        AudioCapture::SystemMix => {
            let source = gst::ElementFactory::make("audiotestsrc")
                .property("is-live", true)
                .property("volume", 0.05_f64)
                .property("freq", 440.0_f64)
                .build()
                .map_err(|error| error.to_string())?;
            pipeline.add(&source).map_err(|error| error.to_string())?;
            Ok(source)
        }
        AudioCapture::Streams { targets } if targets.len() == 1 => {
            let source = application_source(&targets[0])?;
            pipeline.add(&source).map_err(|error| error.to_string())?;
            Ok(source)
        }
        AudioCapture::Streams { targets } => {
            let mixer = gst::ElementFactory::make("audiomixer")
                .build()
                .map_err(|_| "the audio component `audiomixer` is unavailable".to_owned())?;
            pipeline.add(&mixer).map_err(|error| error.to_string())?;
            // Each tapped stream is converted to a common rate and channel
            // layout before the mixer, which requires matching input caps.
            for target in targets {
                let source = application_source(target)?;
                let convert = make_audio("audioconvert")?;
                let resample = make_audio("audioresample")?;
                let caps = gst::ElementFactory::make("capsfilter")
                    .property(
                        "caps",
                        gst::Caps::builder("audio/x-raw")
                            .field("rate", 48_000)
                            .field("channels", 2)
                            .build(),
                    )
                    .build()
                    .map_err(|error| error.to_string())?;
                let leg = [source, convert, resample, caps];
                pipeline.add_many(&leg).map_err(|error| error.to_string())?;
                gst::Element::link_many(&leg).map_err(|error| error.to_string())?;
                leg[leg.len() - 1]
                    .link(&mixer)
                    .map_err(|error| error.to_string())?;
            }
            Ok(mixer)
        }
    }
}

fn application_source(target_object: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make("pipewiresrc")
        .property("target-object", target_object)
        .property("do-timestamp", true)
        .build()
        .map_err(|_| "the audio component `pipewiresrc` is unavailable".to_owned())
}

/// The default capture ceiling, matching the web client's default.
const DEFAULT_CAPTURE_CEILING: (i64, i64) = (2_560, 1_440);

/// Largest even-dimensioned size inside `ceiling` that preserves the source's
/// aspect ratio; sources already inside the ceiling keep their native size.
/// Even dimensions are required by the encoders' chroma subsampling.
fn fit_within_capture_ceiling(width: i32, height: i32, ceiling: (i64, i64)) -> (i32, i32) {
    let (max_width, max_height) = ceiling;
    let width = i64::from(width.max(2));
    let height = i64::from(height.max(2));
    let (scaled_width, scaled_height) = if width <= max_width && height <= max_height {
        (width, height)
    } else {
        let height_at_max_width = (height * max_width + width / 2) / width;
        if height_at_max_width <= max_height {
            (max_width, height_at_max_width)
        } else {
            ((width * max_height + height / 2) / height, max_height)
        }
    };
    (
        (scaled_width & !1).max(2) as i32,
        (scaled_height & !1).max(2) as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_CAPTURE_CEILING, fit_within_capture_ceiling};

    #[test]
    fn scales_common_sources_into_the_ceiling() {
        let fit = |width, height| fit_within_capture_ceiling(width, height, DEFAULT_CAPTURE_CEILING);
        // 4K scales to the ceiling.
        assert_eq!(fit(3840, 2160), (2560, 1440));
        // 16:10 is limited by height, preserving aspect.
        assert_eq!(fit(2560, 1600), (2304, 1440));
        // Ultrawide is limited by width, preserving aspect.
        assert_eq!(fit(5120, 1440), (2560, 720));
        // Sources inside the ceiling keep their native size.
        assert_eq!(fit(1920, 1080), (1920, 1080));
        // A tall window (portrait) is limited by height.
        assert_eq!(fit(1200, 2000), (864, 1440));
        // Odd dimensions are evened for chroma subsampling.
        assert_eq!(fit(1921, 1081), (1920, 1080));
        // Degenerate sizes stay valid.
        assert_eq!(fit(1, 1), (2, 2));
    }

    #[test]
    fn honours_a_configured_ceiling() {
        // A 1440p capture scales into a 1080p ceiling, preserving aspect.
        assert_eq!(
            fit_within_capture_ceiling(2560, 1440, (1920, 1080)),
            (1920, 1080)
        );
        // A source inside the configured ceiling keeps its native size.
        assert_eq!(
            fit_within_capture_ceiling(1280, 720, (1920, 1080)),
            (1280, 720)
        );
    }
}
