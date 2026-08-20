use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
use crate::rate::{self, AdaptiveController, SendRateSampler};
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
/// The MediaStream id signalled for both tracks (the `msid` on each
/// `webrtcbin` sink pad). Left unset, the audio falls back to the RTP cname
/// and the video signals nothing until its ssrc is known, so a browser
/// viewer sees the tracks arrive in two unrelated streams. One shared id
/// groups them into a single remote MediaStream.
const MEDIA_STREAM_ID: &str = "clarity-share";
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

/// A negotiable video codec, in the vocabulary rankings and settings use.
/// The presenter offers every ranked codec its installation can encode; the
/// viewer's answer picks the first it can decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodecId {
    Av1,
    H265,
    H264,
    Vp9,
    Vp8,
}

impl VideoCodecId {
    /// Every codec the engine knows, in the default preference order:
    /// hardware first by quality per bit, software last.
    pub const ALL: [Self; 5] = [Self::Av1, Self::H265, Self::H264, Self::Vp9, Self::Vp8];

    /// The stable lowercase identifier settings persist.
    pub fn id(self) -> &'static str {
        match self {
            Self::Av1 => "av1",
            Self::H265 => "h265",
            Self::H264 => "h264",
            Self::Vp9 => "vp9",
            Self::Vp8 => "vp8",
        }
    }

    /// The display label, matching the RTP encoding name.
    pub fn label(self) -> &'static str {
        VideoCodec::from_id(self).encoding_name()
    }

    /// Parses a persisted identifier; case-insensitive.
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|codec| codec.id().eq_ignore_ascii_case(id.trim()))
    }
}

/// One row of [`video_codec_inventory`]: whether this installation can encode
/// the codec, and whether that encoder is hardware. Settings UIs rank from
/// this so the user sees what a choice costs.
#[derive(Debug, Clone, Copy)]
pub struct VideoCodecCapability {
    pub codec: VideoCodecId,
    pub hardware: bool,
    pub available: bool,
}

/// Probes the installed GStreamer elements for every codec the engine knows,
/// in default preference order.
pub fn video_codec_inventory() -> Vec<VideoCodecCapability> {
    let _ = crate::playback::ensure_gstreamer();
    VideoCodecId::ALL
        .into_iter()
        .map(|id| {
            let codec = VideoCodec::from_id(id);
            VideoCodecCapability {
                codec: id,
                hardware: codec.uses_nvenc(),
                available: codec.is_available(),
            }
        })
        .collect()
}

/// The codec a viewer branch encodes with. Hardware NVENC holds a steady
/// bitrate without CPU cost, which the software encoders cannot at high
/// resolutions; VP8 is the mandatory-to-implement safety net every WebRTC
/// endpoint decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoCodec {
    Av1Nvenc,
    H265Nvenc,
    H264Nvenc,
    Vp9,
    Vp8,
}

impl VideoCodec {
    fn from_id(id: VideoCodecId) -> Self {
        match id {
            VideoCodecId::Av1 => Self::Av1Nvenc,
            VideoCodecId::H265 => Self::H265Nvenc,
            VideoCodecId::H264 => Self::H264Nvenc,
            VideoCodecId::Vp9 => Self::Vp9,
            VideoCodecId::Vp8 => Self::Vp8,
        }
    }

    /// Resolves the user's ranking (or the default order when empty) to the
    /// codecs this installation can actually encode, preserving rank. The
    /// result is what the offer advertises; it always contains at least VP8,
    /// the codec every WebRTC endpoint must decode.
    fn resolve_ranking(ranking: &[VideoCodecId]) -> Vec<Self> {
        let ranked: Vec<Self> = if ranking.is_empty() {
            VideoCodecId::ALL.iter().map(|id| Self::from_id(*id)).collect()
        } else {
            ranking.iter().map(|id| Self::from_id(*id)).collect()
        };
        let mut available: Vec<Self> = ranked
            .into_iter()
            .filter(|codec| codec.is_available())
            .collect();
        if available.is_empty() {
            available.push(Self::Vp8);
        }
        available
    }

    fn is_available(self) -> bool {
        self.required_elements()
            .iter()
            .all(|name| gst::ElementFactory::find(name).is_some())
    }

    /// The RTP encoding name, doubling as the display name in stats.
    fn encoding_name(self) -> &'static str {
        match self {
            Self::Av1Nvenc => "AV1",
            Self::H265Nvenc => "H265",
            Self::H264Nvenc => "H264",
            Self::Vp9 => "VP9",
            Self::Vp8 => "VP8",
        }
    }

    /// The display name reported in per-viewer stats.
    fn label(self) -> &'static str {
        self.encoding_name()
    }

    fn required_elements(self) -> &'static [&'static str] {
        match self {
            Self::Av1Nvenc => &["nvav1enc", "av1parse", "rtpav1pay"],
            Self::H265Nvenc => &["nvh265enc", "h265parse", "rtph265pay"],
            Self::H264Nvenc => &["nvh264enc", "h264parse", "rtph264pay"],
            Self::Vp9 => &["vp9enc", "rtpvp9pay"],
            Self::Vp8 => &["vp8enc", "rtpvp8pay"],
        }
    }

    /// The raw pixel format the encoder consumes. The shared path normalizes
    /// to the top-ranked codec's format; a branch whose negotiated codec
    /// wants another format converts locally.
    fn raw_format(self) -> &'static str {
        match self {
            Self::Av1Nvenc | Self::H265Nvenc | Self::H264Nvenc => "NV12",
            Self::Vp9 | Self::Vp8 => "I420",
        }
    }

    fn uses_nvenc(self) -> bool {
        matches!(self, Self::Av1Nvenc | Self::H265Nvenc | Self::H264Nvenc)
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

    /// The `application/x-rtp` structure advertising this codec at `pt`, as
    /// used both in the branch's capsfilter and in the transceiver's
    /// codec-preferences that make the offer multi-codec.
    fn rtp_structure(self, pt: u32) -> gst::Structure {
        let mut structure = gst::Structure::builder("application/x-rtp")
            .field("media", "video")
            .field("encoding-name", self.encoding_name())
            .field("clock-rate", 90_000)
            .field("payload", i32::try_from(pt).unwrap_or(96))
            .field("rtcp-fb-nack-pli", true);
        // The transport-wide congestion control extension carries the feedback
        // the GCC estimator consumes; without the local element the offer stays
        // honest and adaptation simply never engages.
        if gst::ElementFactory::find("rtphdrexttwcc").is_some() {
            structure = structure
                .field("rtcp-fb-transport-cc", true)
                .field("extmap-3", TWCC_EXTENSION_URI);
        }
        structure.build()
    }

    /// Builds the encode-to-RTP chain (encoder … capsfilter) and returns it
    /// with the encoder element for live bitrate control. The per-viewer branch
    /// links queue → valve into the first element and the last into webrtcbin.
    fn build_encode(
        self,
        initial_kbps: u32,
        pt: u32,
    ) -> Result<(Vec<gst::Element>, gst::Element), String> {
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
            // Quality per bit. The p6 preset spends more GPU on each frame
            // (hardware NVENC keeps that comfortably realtime at screen-share
            // resolutions), spatial AQ shifts bits toward the detailed
            // regions text lives in, and the quarter-resolution first pass
            // lets the second pass place bits where the frame needs them.
            // Guarded per property: older nvcodec builds lack some of these,
            // and the encoder works without them.
            if encoder.find_property("preset").is_some() {
                encoder.set_property_from_str("preset", "p6");
            }
            if encoder.find_property("spatial-aq").is_some() {
                encoder.set_property("spatial-aq", true);
            }
            if encoder.find_property("multi-pass").is_some() {
                encoder.set_property_from_str("multi-pass", "two-pass-quarter");
            }
            encoder.set_property("bitrate", initial_kbps);
            set_max_bitrate(encoder, initial_kbps);
        };
        // Realtime software encoding, shared by VP8 and VP9: deadline=1
        // selects realtime mode; PLI keyframes arrive as upstream
        // force-keyunit events.
        let configure_vpx = |encoder: &gst::Element| {
            encoder.set_property("deadline", 1i64);
            encoder.set_property_from_str("end-usage", "cbr");
            encoder.set_property(
                "target-bitrate",
                i32::try_from(initial_kbps.saturating_mul(1000)).unwrap_or(i32::MAX),
            );
            encoder.set_property("cpu-used", 8i32);
            encoder.set_property("threads", 4i32);
        };
        let (encoder, middle): (gst::Element, Vec<gst::Element>) = match self {
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
                pay.set_property("pt", pt);
                // Repeat SPS/PPS on every keyframe so a viewer that joins or
                // recovers mid-stream can start decoding without waiting.
                pay.set_property("config-interval", -1i32);
                pay.set_property_from_str("aggregate-mode", "zero-latency");
                (encoder, vec![profile, parse, pay])
            }
            Self::H265Nvenc => {
                let encoder = make("nvh265enc")?;
                configure_nvenc(&encoder);
                // Main profile is the widest H.265 decode support (Safari's
                // hardware path included); parse/payloader repeat VPS/SPS/PPS
                // on keyframes for mid-stream joins, as with H.264.
                let profile = gst::ElementFactory::make("capsfilter")
                    .property(
                        "caps",
                        gst::Caps::builder("video/x-h265")
                            .field("profile", "main")
                            .build(),
                    )
                    .build()
                    .map_err(|_| "`capsfilter` is unavailable".to_owned())?;
                let parse = make("h265parse")?;
                parse.set_property("config-interval", -1i32);
                let pay = make("rtph265pay")?;
                pay.set_property("pt", pt);
                pay.set_property("config-interval", -1i32);
                pay.set_property_from_str("aggregate-mode", "zero-latency");
                (encoder, vec![profile, parse, pay])
            }
            Self::Av1Nvenc => {
                let encoder = make("nvav1enc")?;
                configure_nvenc(&encoder);
                let parse = make("av1parse")?;
                let pay = make("rtpav1pay")?;
                pay.set_property("pt", pt);
                (encoder, vec![parse, pay])
            }
            Self::Vp9 => {
                let encoder = make("vp9enc")?;
                configure_vpx(&encoder);
                let pay = make("rtpvp9pay")?;
                pay.set_property("pt", pt);
                pay.set_property_from_str("picture-id-mode", "15-bit");
                (encoder, vec![pay])
            }
            Self::Vp8 => {
                let encoder = make("vp8enc")?;
                configure_vpx(&encoder);
                let pay = make("rtpvp8pay")?;
                pay.set_property("pt", pt);
                pay.set_property_from_str("picture-id-mode", "15-bit");
                (encoder, vec![pay])
            }
        };
        let caps = gst::ElementFactory::make("capsfilter")
            .property("caps", {
                let mut caps = gst::Caps::new_empty();
                caps.get_mut()
                    .expect("caps are not yet shared")
                    .append_structure(self.rtp_structure(pt));
                caps
            })
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
    /// real capture, a soft test tone for the synthetic source, and silence
    /// while idle. The head follows the video source across swaps, so a
    /// broadcast opened idle picks up the real monitor when sharing starts.
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
    /// The ranked codecs the offer advertises, best first; an empty ranking
    /// means the default order ([`VideoCodecId::ALL`]). Codecs whose encoder
    /// is not installed are skipped; VP8 backstops an empty result.
    pub video_codecs: Vec<VideoCodecId>,
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
    /// The profile's frame rate, pinned into the normalize caps so the
    /// stream is constant-rate into the encoder; re-applied on source swaps.
    frame_rate: i32,
    /// The live capture backing the current source; dropping it revokes the
    /// compositor stream, so it is swapped together with the source head and
    /// released entirely while idle.
    capture: Mutex<Option<CaptureStream>>,
    /// The replaceable audio-capture elements, output element last, up to
    /// (not including) the fixed audio tail. Empty when the broadcast has no
    /// audio chain.
    audio_head: Mutex<Vec<gst::Element>>,
    /// First element of the fixed audio tail (`audioconvert`); a replacement
    /// audio head links into it. `None` when the broadcast has no audio.
    audio_tail_input: Option<gst::Element>,
    /// What the audio head captures when a source is live, rebuilt on every
    /// source swap.
    audio_config: AudioCapture,
    /// The reconcilable mixer state while a per-stream head is live; `None`
    /// while idle, for monitor capture, or without audio.
    audio_mix: Mutex<Option<AudioMixState>>,
    /// The most recently requested stream targets, applied to the head on
    /// every rebuild so a swap back from idle resumes the current mix, not
    /// the one from start.
    audio_targets: Mutex<Vec<String>>,
}

/// Which audio head accompanies the current video source. The idle
/// placeholder is always silence and holds no capture of any kind; the
/// synthetic source keeps its soft test tone for `SystemMix` so the audio
/// path stays audible in development.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AudioHeadMode {
    Capture,
    Synthetic,
    Idle,
}

impl AudioHeadMode {
    fn of(source: &SourceConfig) -> Self {
        match source {
            SourceConfig::Screen(_) => Self::Capture,
            SourceConfig::Synthetic(_) => Self::Synthetic,
            SourceConfig::Idle => Self::Idle,
        }
    }
}

struct Shared {
    events: mpsc::UnboundedSender<BroadcastEvent>,
    viewers: Mutex<HashMap<String, ViewerEntry>>,
    /// Server-known display names by peer id. Chat relayed from a viewer is
    /// stamped with this name, never with the sender the payload claims.
    display_names: Mutex<HashMap<String, String>>,
    ice: Mutex<IceEndpoints>,
    /// The offered codec ranking; index `i` is advertised at payload type
    /// `96 + i`. The top entry is the branch built before negotiation and
    /// defines the shared normalize format.
    codecs: Vec<VideoCodec>,
    force_relay: bool,
    paused: AtomicBool,
    ended: AtomicBool,
    /// Names of elements inside removable audio legs. A pipeline error from
    /// one of these (an application stream vanishing mid-share) downgrades
    /// that leg instead of ending the broadcast; the next reconcile removes
    /// it.
    audio_leg_names: Mutex<std::collections::HashSet<String>>,
    /// Branch elements of viewers whose teardown is in flight. A collapsing
    /// connection keeps posting errors between its removal from `viewers`
    /// and the branch leaving the pipeline; tracking the elements here keeps
    /// those stragglers attributable so they cannot end the share.
    dismantling: Mutex<Vec<gst::Element>>,
}

/// Registers a mix's leg elements for bus-error containment.
fn register_leg_names(shared: &Arc<Shared>, mix: &AudioMixState) {
    let mut names = shared.audio_leg_names.lock().expect("leg name lock");
    for leg in mix.legs.values() {
        for element in leg {
            names.insert(element.name().to_string());
        }
    }
}

impl Shared {
    /// The top-ranked offered codec: the branch built ahead of negotiation,
    /// and the format the shared normalize path produces.
    fn top_codec(&self) -> VideoCodec {
        self.codecs[0]
    }
}

/// What installing a negotiated video branch produced: every element that
/// joined the pipeline, plus the head/pad handles when the install created
/// them (the attach path) rather than reusing existing ones (the swap path).
struct InstalledVideoBranch {
    branch_elements: Vec<gst::Element>,
    valve: Option<gst::Element>,
    tee_pad: Option<gst::Pad>,
    sink_pad: Option<gst::Pad>,
}

struct ViewerEntry {
    webrtc: gst::Element,
    /// Present once the answer's codec pick attached the video branch.
    video_valve: Option<gst::Element>,
    audio_valve: Option<gst::Element>,
    branch: Vec<gst::Element>,
    /// Present once the video branch is attached.
    tee_pad: Option<gst::Pad>,
    audio_tee_pad: Option<gst::Pad>,
    remote_description_set: bool,
    queued_candidates: Vec<(u32, String)>,
    stats_baseline: Option<StatsBaseline>,
    /// The video encoder's current target in kbps — the estimator's decision
    /// when adaptive, or the fixed ceiling. Written from the estimator
    /// callback and read by the stats reporter, so it is lock-free.
    target_kbps: Arc<AtomicU32>,
    /// Total video RTP bytes handed to the connection, counted by a pad probe
    /// on the video sink pad. The adaptive controller derives its send-rate
    /// reading from this at the estimator's cadence; the 2-second stats poll
    /// is far too stale to classify the application-limited region around
    /// busy/static transitions.
    video_bytes_sent: Arc<AtomicU64>,
    /// The codec this viewer's branch encodes with: the top-ranked codec
    /// until the answer arrives, the negotiated codec after.
    codec: VideoCodec,
    /// The encode chain (encoder … capsfilter) between the valve and the
    /// connection; empty until the answer picks the codec, replaced if a
    /// later answer picks another.
    encode: Vec<gst::Element>,
    /// The connection's video sink pad, present once the branch attached.
    video_sink_pad: Option<gst::Pad>,
    /// What live rate control drives, shared with the GCC callback so a
    /// codec swap redirects it to the replacement encoder.
    rate_target: RateTarget,
    /// The reliable data channel carrying chat with this viewer.
    chat: Option<gst_webrtc::WebRTCDataChannel>,
}

/// The encoder live rate control drives, and the codec whose property scheme
/// applies. `None` until the answer's codec pick attaches the encode chain;
/// swapped together with it.
type RateTarget = Arc<Mutex<Option<(VideoCodec, gst::Element)>>>;

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
        let codecs = VideoCodec::resolve_ranking(&config.video_codecs);
        let video_codec = codecs[0];
        tracing::info!(offered = ?codecs, "encoding video");
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
            codecs,
            force_relay: config.force_relay,
            paused: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            audio_leg_names: Mutex::new(std::collections::HashSet::new()),
            dismantling: Mutex::new(Vec::new()),
        });

        let pipeline = gst::Pipeline::new();
        // The capture source offers the compositor's PipeWire clock, which the
        // pipeline would otherwise adopt — and in some sessions that clock
        // never advances, freezing every buffer timestamp at zero so all but
        // the first frame is discarded downstream as a duplicate. Frame
        // timestamps must come from the same clock the RTP stack paces by.
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        let start_error = |error: gst::glib::BoolError| BroadcastError::Start(error.to_string());
        let audio_mode = AudioHeadMode::of(&config.source);
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
        // Normalize to a constant frame rate at the profile's target: a
        // 60/120 Hz capture is thinned to the cap, and a static screen's
        // occasional keepalive frames are duplicated up to it. The
        // duplication is what lets the encoder converge on quality:
        // compositors only deliver frames on damage, NVENC budgets bits per
        // frame, and a screen that stops moving right after a scroll would
        // otherwise keep its last motion-quality frame nearly forever,
        // receiving one small refinement installment per keepalive. At a
        // constant rate the idle bitrate budget is spent sharpening within a
        // second, after which duplicated frames encode as skips that cost
        // almost nothing.
        let frame_rate = config.frame_rate.clamp(1, 120) as i32;
        let videorate = gst::ElementFactory::make("videorate")
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
                    .field("framerate", gst::Fraction::new(frame_rate, 1))
                    .build(),
            )
            .build()
            .map_err(start_error)?;
        let capture_ceiling = config
            .capture_ceiling
            .map_or(DEFAULT_CAPTURE_CEILING, |(width, height)| {
                (i64::from(width.max(2)), i64::from(height.max(2)))
            });
        wire_caps_notify(&head, &normalize, raw_format, capture_ceiling, frame_rate)?;
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

        let audio_chain = match build_audio_chain(&pipeline, &config.audio, audio_mode) {
            Ok(chain) => chain,
            Err(reason) => {
                tracing::warn!(%reason, "sharing without audio");
                None
            }
        };
        let (audio_tee, audio_head, audio_tail_input, audio_mix) = match audio_chain {
            Some(chain) => (
                Some(chain.tee),
                chain.head,
                Some(chain.tail_input),
                chain.mix,
            ),
            None => (None, Vec::new(), None, None),
        };
        if let Some(mix) = &audio_mix {
            register_leg_names(&shared, mix);
        }
        let audio_targets = match &config.audio {
            AudioCapture::Streams { targets } => targets.clone(),
            _ => Vec::new(),
        };

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|error| BroadcastError::Start(error.to_string()))?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let bus_thread =
            spawn_bus_thread(&pipeline, &tee, audio_tee.as_ref(), &shared, &shutdown)?;

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
                frame_rate,
                capture: Mutex::new(capture),
                audio_head: Mutex::new(audio_head),
                audio_tail_input,
                audio_config: config.audio,
                audio_mix: Mutex::new(audio_mix),
                audio_targets: Mutex::new(audio_targets),
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
        let audio_mode = AudioHeadMode::of(&source);
        let (new_head, new_capture) = build_source_head(source)?;
        let raw_format = self.shared.top_codec().raw_format();
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
            wire_caps_notify(
                &new_head,
                &self.normalize,
                raw_format,
                self.capture_ceiling,
                self.frame_rate,
            )?;
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
        // The audio head follows the video source: real capture audio only
        // while a capture is live, silence while idle. Without this the head
        // picked at start (silence for an idle-opened room) would keep
        // playing for the whole broadcast.
        self.replace_audio_head(audio_mode);
        Ok(())
    }

    /// Swaps the audio head to match the current source, mirroring the video
    /// swap: park, unlink, dismantle, rebuild, relink. A head that cannot be
    /// built (a missing audio server, a vanished application stream) degrades
    /// to the silent placeholder rather than failing the source swap — the
    /// video went through, and audio downgrade is this crate's contract.
    fn replace_audio_head(&self, mode: AudioHeadMode) {
        let Some(tail_input) = &self.audio_tail_input else {
            return;
        };
        let mut head = self.audio_head.lock().expect("audio head lock");
        let old_head = std::mem::take(&mut *head);
        // The old mix (if any) is dismantled with the head; its legs stop
        // being containment-tracked.
        *self.audio_mix.lock().expect("audio mix lock") = None;
        self.shared
            .audio_leg_names
            .lock()
            .expect("leg name lock")
            .clear();
        if let Some(head_src) = old_head.last().and_then(|element| element.static_pad("src")) {
            // Same parking dance as the video head: the streaming thread must
            // be off the tail before anything is unlinked.
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
        // Null downstream-first, exactly as in `replace_source`: flushing the
        // probed pad releases the parked thread so upstream elements can stop.
        for element in old_head.iter().rev() {
            let _ = element.set_state(gst::State::Null);
        }
        let _ = self.pipeline.remove_many(&old_head);

        // A per-stream head rebuilds against the CURRENT targets, so a swap
        // back from idle resumes the reconciled mix, not the one from start.
        let audio_config = match &self.audio_config {
            AudioCapture::Streams { .. } => AudioCapture::Streams {
                targets: self.audio_targets.lock().expect("audio target lock").clone(),
            },
            other => other.clone(),
        };
        let attach = |mode: AudioHeadMode| -> Result<BuiltAudioHead, String> {
            let built = build_audio_head(&self.pipeline, &audio_config, mode)?;
            let result = (|| {
                let output = built.elements.last().ok_or("the audio head is empty")?;
                output.link(tail_input).map_err(|error| error.to_string())?;
                for element in built.elements.iter().rev() {
                    element
                        .sync_state_with_parent()
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            })();
            match result {
                Ok(()) => Ok(built),
                Err(reason) => {
                    for element in built.elements.iter().rev() {
                        let _ = element.set_state(gst::State::Null);
                    }
                    let _ = self.pipeline.remove_many(&built.elements);
                    Err(reason)
                }
            }
        };
        match attach(mode).or_else(|reason| {
            tracing::warn!(%reason, "audio capture unavailable; sending silence");
            attach(AudioHeadMode::Idle)
        }) {
            Ok(built) => {
                *head = built.elements;
                if let Some(mix) = &built.mix {
                    register_leg_names(&self.shared, mix);
                }
                *self.audio_mix.lock().expect("audio mix lock") = built.mix;
            }
            Err(reason) => {
                // No head at all: the tail runs starved and viewers get
                // silence. Nothing to dismantle later.
                tracing::warn!(%reason, "the audio head could not be rebuilt; audio is silent");
            }
        }
    }

    /// Reconciles the live per-stream audio mix against `targets`: legs for
    /// new targets are added, legs whose target is gone are dismantled, and
    /// the rest keep flowing. The requested set also becomes the mix a
    /// return from idle rebuilds. On a broadcast without a per-stream mix
    /// (no audio, monitor capture, or currently idle) only the stored
    /// targets change. This is the mechanism behind the presenter's audio
    /// watchdog: the excluded application stays out of the mix however late
    /// it starts playing, and applications that begin playing mid-share
    /// become audible without renegotiation.
    pub fn set_audio_streams(&self, targets: &[String]) {
        *self.audio_targets.lock().expect("audio target lock") = targets.to_vec();
        let mut mix = self.audio_mix.lock().expect("audio mix lock");
        let Some(mix) = mix.as_mut() else {
            return;
        };
        let head_holds = |head: &mut Vec<gst::Element>, leg: &[gst::Element], keep: bool| {
            if keep {
                head.extend(leg.iter().cloned());
            } else {
                head.retain(|element| !leg.contains(element));
            }
        };
        let mut head = self.audio_head.lock().expect("audio head lock");

        let stale: Vec<String> = mix
            .legs
            .keys()
            .filter(|target| !targets.contains(target))
            .cloned()
            .collect();
        for target in stale {
            let Some(leg) = mix.legs.remove(&target) else {
                continue;
            };
            {
                let mut names = self.shared.audio_leg_names.lock().expect("leg name lock");
                for element in &leg {
                    names.remove(element.name().as_str());
                }
            }
            // Park the leg's streaming thread at its output, exactly as
            // `replace_source` parks the capture head, then release the
            // mixer pad and dismantle downstream-first (flushing the probed
            // pad frees the parked thread so the source can stop).
            if let Some(leg_src) = leg.last().and_then(|element| element.static_pad("src")) {
                let (parked, wait_parked) = std::sync::mpsc::channel::<()>();
                leg_src.add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, move |_, _| {
                    let _ = parked.send(());
                    gst::PadProbeReturn::Ok
                });
                let _ = wait_parked.recv_timeout(Duration::from_secs(1));
                if let Some(peer) = leg_src.peer() {
                    let _ = leg_src.unlink(&peer);
                    mix.mixer.release_request_pad(&peer);
                }
            }
            for element in leg.iter().rev() {
                let _ = element.set_state(gst::State::Null);
            }
            let _ = self.pipeline.remove_many(&leg);
            head_holds(&mut head, &leg, false);
            tracing::info!(%target, "an audio stream left the shared mix");
        }

        for target in targets {
            if mix.legs.contains_key(target) {
                continue;
            }
            match build_stream_leg(&self.pipeline, &mix.mixer, target) {
                Ok(leg) => {
                    {
                        let mut names =
                            self.shared.audio_leg_names.lock().expect("leg name lock");
                        for element in &leg {
                            names.insert(element.name().to_string());
                        }
                    }
                    head_holds(&mut head, &leg, true);
                    mix.legs.insert(target.clone(), leg);
                    tracing::info!(%target, "an audio stream joined the shared mix");
                }
                Err(reason) => {
                    tracing::warn!(%target, %reason, "an audio stream could not join the mix");
                }
            }
        }
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
        // Congestion control is delegated to a GCC estimator, attached to the
        // connection through webrtcbin's aux-sender hook; without the
        // element the viewer holds the ceiling. `claritygccbwe` is the
        // vendored element `ensure_gstreamer` registers, so it is available
        // whenever GStreamer itself initialized; the `rtpgccbwe` check
        // remains as a fallback for a caller that skipped that step.
        let adaptive_bwe = encoding.adaptive
            && (gst::ElementFactory::find("claritygccbwe").is_some()
                || gst::ElementFactory::find("rtpgccbwe").is_some());
        if encoding.adaptive && !adaptive_bwe {
            tracing::warn!("no GCC bandwidth estimator is available; this viewer holds a fixed bitrate");
        }
        let initial_kbps = if adaptive_bwe {
            start_video_kbps(encoding.bitrate_kbps)
        } else {
            encoding.bitrate_kbps
        };
        let target_kbps = Arc::new(AtomicU32::new(initial_kbps));
        let video_bytes_sent = Arc::new(AtomicU64::new(0));
        // The branch reports the top-ranked codec until the answer's pick
        // attaches the real encoder.
        let codec = self.shared.top_codec();
        let rate_target: RateTarget = Arc::new(Mutex::new(None));
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

        let mut branch = vec![webrtc.clone()];
        self.pipeline
            .add(&webrtc)
            .map_err(|error| BroadcastError::Viewer(error.to_string()))?;
        // The video m-line is declared as a detached transceiver carrying the
        // whole codec ranking; no encoder exists yet. A pad linked here would
        // pin the m-line to its single codec (webrtcbin intersects
        // codec-preferences with pad caps), so the encode branch attaches
        // only once the answer picks — see `apply_negotiated_codec`.
        let video_transceiver = webrtc.emit_by_name::<gst_webrtc::WebRTCRTPTransceiver>(
            "add-transceiver",
            &[
                &gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly,
                &offered_caps(&self.shared.codecs),
            ],
        );
        video_transceiver.set_property("do-nack", true);

        // The audio leg feeds the same connection as a second stream.
        let audio_leg_input = if self.audio_tee.is_some() {
            let audio_queue = gst::ElementFactory::make("queue")
                .property_from_str("leaky", "downstream")
                .property("max-size-buffers", 8u32)
                .property("max-size-bytes", 0u32)
                .property("max-size-time", 0u64)
                .build()
                .map_err(|_| BroadcastError::Viewer("`queue` is unavailable".into()))?;
            // The valve serves pause only. While idle the head itself is
            // silence with no capture behind it, and the silence must flow:
            // a valve shut from birth starves the encoder of buffers, caps
            // never reach `webrtcbin`'s audio pad, and the offer for a
            // viewer joining an idle room is never created.
            let audio_valve = build("valve")?;
            audio_valve.set_property("drop", self.shared.paused.load(Ordering::SeqCst));
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
                // The offer is created as soon as the connection is assembled,
                // usually before the first buffer reaches this branch, so the
                // m-line is built from these query-time caps. Without the
                // channel count the rtpmap comes out as `OPUS/48000`, and
                // browsers, which implement RFC 7587's `opus/48000/2`
                // verbatim, reject the whole audio section.
                .field("encoding-params", "2")
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
            // Explicitly m-line 1: an unnumbered request would bind the
            // detached video transceiver at index 0.
            let audio_sink = webrtc.request_pad_simple("sink_1").ok_or_else(|| {
                BroadcastError::Viewer("the connection rejected an audio stream".into())
            })?;
            audio_sink.set_property("msid", MEDIA_STREAM_ID);
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
                Arc::clone(&rate_target),
                encoding.bitrate_kbps,
                self.audio_tee.is_some(),
                Arc::clone(&target_kbps),
                Arc::clone(&video_bytes_sent),
            );
        }
        if audio_queue.is_some()
            && let Some(transceiver) = webrtc
                .emit_by_name::<Option<gst_webrtc::WebRTCRTPTransceiver>>(
                    "get-transceiver",
                    &[&1i32],
                )
        {
            transceiver.set_property(
                "direction",
                gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly,
            );
        }

        for element in branch.iter().rev() {
            element
                .sync_state_with_parent()
                .map_err(|error| BroadcastError::Viewer(error.to_string()))?;
        }
        let webrtc_handle = webrtc.clone();
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
                video_valve: None,
                audio_valve,
                branch,
                tee_pad: None,
                audio_tee_pad,
                remote_description_set: false,
                queued_candidates: Vec::new(),
                stats_baseline: None,
                target_kbps,
                video_bytes_sent,
                codec,
                encode: Vec::new(),
                video_sink_pad: None,
                rate_target,
                chat,
            },
        );
        // With the video m-line declared as a detached transceiver, webrtcbin
        // never fires on-negotiation-needed (it only checks on pad and
        // description changes), so the initial offer is kicked explicitly
        // once the connection is fully assembled.
        negotiate(&webrtc_handle, &self.shared, peer_id, false);
        Ok(())
    }

    /// Gives a viewer the encode branch its answer negotiated. On the first
    /// answer this attaches the whole video branch (tee pad, queue, valve,
    /// encoder chain, connection pad) — no encoder exists before the pick is
    /// known. A later answer picking a different codec (never expected once
    /// preferences narrow, but legal SDP) swaps the encode chain in place. A
    /// failure leaves the viewer without video (audio and chat continue)
    /// rather than failing the session; the client's escalation rebuild
    /// recovers it.
    fn apply_negotiated_codec(&self, peer_id: &str, pt: u32, codec: VideoCodec) {
        let viewers = self.shared.viewers.lock().expect("viewer lock");
        let Some(entry) = viewers.get(peer_id) else {
            return;
        };
        let attached = !entry.encode.is_empty();
        if attached && entry.codec == codec {
            return;
        }
        let webrtc = entry.webrtc.clone();
        let valve = entry.video_valve.clone();
        let old = entry.encode.clone();
        let sink_pad = entry.video_sink_pad.clone();
        let video_bytes_sent = Arc::clone(&entry.video_bytes_sent);
        let target_kbps = entry.target_kbps.load(Ordering::Relaxed);
        let paused = self.shared.paused.load(Ordering::SeqCst);
        drop(viewers);

        // Build the encode chain first; nothing is dismantled on failure.
        let built = (|| -> Result<(Vec<gst::Element>, gst::Element), String> {
            let (chain, encoder) = codec.build_encode(target_kbps, pt)?;
            let mut elements = Vec::new();
            // The shared path normalizes to the top codec's raw format; a
            // negotiated codec wanting another format converts locally.
            if codec.raw_format() != self.shared.top_codec().raw_format() {
                let convert = gst::ElementFactory::make("videoconvert")
                    .build()
                    .map_err(|_| "`videoconvert` is unavailable".to_owned())?;
                elements.push(convert);
            }
            elements.extend(chain);
            Ok((elements, encoder))
        })();
        let (elements, encoder) = match built {
            Ok(built) => built,
            Err(reason) => {
                tracing::warn!(
                    peer = peer_id,
                    %reason,
                    "the negotiated codec's encoder could not be built; this viewer gets no video"
                );
                return;
            }
        };

        let installed = if attached {
            self.swap_encode_chain(&valve, &old, &sink_pad, &elements)
        } else {
            self.attach_video_branch(&webrtc, paused, &elements, video_bytes_sent)
        };
        match installed {
            Ok(installed) => {
                // The renegotiation the pad request triggers is usually
                // created before the first buffer traverses the new branch,
                // so the video m-line goes out without its ssrc, and with it
                // goes the msid that groups both tracks into one stream for
                // browsers. Re-offer once when the caps actually reach the
                // connection pad; the ssrc and `msid` are known by then.
                if let Some(pad) = &installed.sink_pad {
                    let shared = Arc::clone(&self.shared);
                    let peer = peer_id.to_owned();
                    let webrtc_hook = webrtc.clone();
                    let offered = Arc::new(AtomicBool::new(false));
                    let offered_hook = Arc::clone(&offered);
                    pad.connect_notify(Some("caps"), move |pad, _| {
                        if pad.current_caps().is_none()
                            || offered_hook.swap(true, Ordering::SeqCst)
                        {
                            return;
                        }
                        negotiate(&webrtc_hook, &shared, &peer, false);
                    });
                    // The caps can land between linking and connecting the
                    // handler; offer now if they already did.
                    if pad.current_caps().is_some()
                        && !offered.swap(true, Ordering::SeqCst)
                    {
                        negotiate(&webrtc, &self.shared, peer_id, false);
                    }
                }
                let mut viewers = self.shared.viewers.lock().expect("viewer lock");
                if let Some(entry) = viewers.get_mut(peer_id) {
                    *entry.rate_target.lock().expect("rate target lock") =
                        Some((codec, encoder.clone()));
                    entry.branch.retain(|element| !old.contains(element));
                    entry.branch.extend(installed.branch_elements.iter().cloned());
                    entry.encode = elements;
                    entry.codec = codec;
                    if let Some(valve) = installed.valve {
                        entry.video_valve = Some(valve);
                    }
                    if let Some(pad) = installed.tee_pad {
                        entry.tee_pad = Some(pad);
                    }
                    if let Some(pad) = installed.sink_pad {
                        entry.video_sink_pad = Some(pad);
                    }
                } else {
                    // The viewer left mid-install (the bus watch can dismantle
                    // a dying viewer concurrently now): its teardown snapshot
                    // predates these pads, so take the fresh branch down here,
                    // unlinking the tee tap under an idle probe exactly as
                    // `dismantle_viewer` would, and releasing the request pads
                    // that would otherwise stay linked to a removed branch.
                    self.shared
                        .dismantling
                        .lock()
                        .expect("dismantling lock")
                        .extend(installed.branch_elements.iter().cloned());
                    let pipeline = self.pipeline.clone();
                    let elements = installed.branch_elements.clone();
                    let webrtc_pad = installed.sink_pad.clone();
                    let webrtc = webrtc.clone();
                    let take_down = move || {
                        for element in elements.iter().rev() {
                            let _ = element.set_state(gst::State::Null);
                        }
                        let _ = pipeline.remove_many(&elements);
                        if let Some(pad) = &webrtc_pad {
                            webrtc.release_request_pad(pad);
                        }
                    };
                    match &installed.tee_pad {
                        Some(tee_pad) => {
                            let tee = self.tee.clone();
                            tee_pad.add_probe(gst::PadProbeType::IDLE, move |pad, _| {
                                if let Some(peer) = pad.peer() {
                                    let _ = pad.unlink(&peer);
                                }
                                tee.release_request_pad(pad);
                                take_down();
                                gst::PadProbeReturn::Remove
                            });
                        }
                        None => take_down(),
                    }
                }
            }
            Err(reason) => tracing::warn!(
                peer = peer_id,
                %reason,
                "the negotiated video branch could not be installed; this viewer gets no video"
            ),
        }
    }

    /// First-answer path: builds the branch head (queue, valve), links the
    /// encode chain into the connection's declared video m-line, and taps the
    /// source tee last so frames only flow into a complete branch.
    fn attach_video_branch(
        &self,
        webrtc: &gst::Element,
        paused: bool,
        encode: &[gst::Element],
        video_bytes_sent: Arc<AtomicU64>,
    ) -> Result<InstalledVideoBranch, String> {
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
            .map_err(|_| "`queue` is unavailable".to_owned())?;
        let valve = gst::ElementFactory::make("valve")
            .build()
            .map_err(|_| "`valve` is unavailable".to_owned())?;
        valve.set_property("drop", paused);
        let mut elements = vec![queue.clone(), valve.clone()];
        elements.extend(encode.iter().cloned());
        self.pipeline
            .add_many(&elements)
            .map_err(|error| error.to_string())?;
        gst::Element::link_many(&elements).map_err(|error| error.to_string())?;
        // The declared video m-line's pad; requested by name because the
        // transceiver was added detached at index 0.
        let sink_pad = webrtc
            .request_pad_simple("sink_0")
            .ok_or("the connection rejected the video stream")?;
        sink_pad.set_property("msid", MEDIA_STREAM_ID);
        // Count the video RTP bytes entering the connection; the adaptive
        // controller reads its send rate off this counter at the estimator's
        // cadence. The connection pad outlives codec swaps, so the counter
        // runs for the branch's whole life.
        sink_pad.add_probe(
            gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
            move |_, info| {
                let bytes = match &info.data {
                    Some(gst::PadProbeData::Buffer(buffer)) => buffer.size() as u64,
                    Some(gst::PadProbeData::BufferList(list)) => {
                        list.iter().map(|buffer| buffer.size() as u64).sum()
                    }
                    _ => 0,
                };
                if bytes > 0 {
                    video_bytes_sent.fetch_add(bytes, Ordering::Relaxed);
                }
                gst::PadProbeReturn::Ok
            },
        );
        elements
            .last()
            .expect("the encode chain is never empty")
            .static_pad("src")
            .expect("capsfilter has a src pad")
            .link(&sink_pad)
            .map_err(|_| "the encoder could not reach the connection".to_owned())?;
        for element in elements.iter().rev() {
            element
                .sync_state_with_parent()
                .map_err(|error| error.to_string())?;
        }
        let tee_pad = self
            .tee
            .request_pad_simple("src_%u")
            .ok_or("the source could not add another viewer")?;
        tee_pad
            .link(&queue.static_pad("sink").expect("queue has a sink pad"))
            .map_err(|_| "the source could not reach the encoder".to_owned())?;
        Ok(InstalledVideoBranch {
            branch_elements: elements,
            valve: Some(valve),
            tee_pad: Some(tee_pad),
            sink_pad: Some(sink_pad),
        })
    }

    /// Later-answer path: replaces the encode chain between the existing
    /// valve and connection pad. The branch's streaming thread is parked
    /// under a blocking probe at the valve, exactly as `replace_source` parks
    /// the capture head; unlike there the probed pad outlives the swap, so
    /// the probe is removed afterwards.
    fn swap_encode_chain(
        &self,
        valve: &Option<gst::Element>,
        old: &[gst::Element],
        sink_pad: &Option<gst::Pad>,
        elements: &[gst::Element],
    ) -> Result<InstalledVideoBranch, String> {
        let valve = valve.as_ref().ok_or("the video branch has no valve")?;
        let sink_pad = sink_pad
            .as_ref()
            .ok_or("the video branch has no connection pad")?;
        let valve_src = valve.static_pad("src").expect("valve has a src pad");
        let (parked, wait_parked) = std::sync::mpsc::channel::<()>();
        let probe = valve_src.add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, move |_, _| {
            let _ = parked.send(());
            gst::PadProbeReturn::Ok
        });
        let _ = wait_parked.recv_timeout(Duration::from_secs(1));
        if let Some(first) = old.first() {
            let _ = valve_src.unlink(&first.static_pad("sink").expect("the chain has a sink pad"));
        }
        if let Some(last) = old.last() {
            let _ = last
                .static_pad("src")
                .expect("the chain has a src pad")
                .unlink(sink_pad);
        }
        for element in old.iter().rev() {
            let _ = element.set_state(gst::State::Null);
        }
        let _ = self.pipeline.remove_many(old);

        let installed = (|| -> Result<(), String> {
            self.pipeline
                .add_many(elements)
                .map_err(|error| error.to_string())?;
            gst::Element::link_many(elements).map_err(|error| error.to_string())?;
            valve
                .link(&elements[0])
                .map_err(|error| error.to_string())?;
            elements
                .last()
                .expect("the encode chain is never empty")
                .static_pad("src")
                .expect("capsfilter has a src pad")
                .link(sink_pad)
                .map_err(|_| "the encoder could not reach the connection".to_owned())?;
            for element in elements.iter().rev() {
                element
                    .sync_state_with_parent()
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })();
        if let Some(probe) = probe {
            valve_src.remove_probe(probe);
        }
        if let Err(reason) = installed {
            // A failed swap must not strand the half-installed chain in the
            // pipeline; the old chain is already gone, so the viewer ends up
            // with no video branch, which the caller reports.
            for element in elements.iter().rev() {
                let _ = element.set_state(gst::State::Null);
            }
            let _ = self.pipeline.remove_many(elements);
            return Err(reason);
        }
        Ok(InstalledVideoBranch {
            branch_elements: elements.to_vec(),
            valve: None,
            tee_pad: None,
            sink_pad: None,
        })
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
        remove_viewer_branch(
            &self.shared,
            &self.pipeline,
            &self.tee,
            self.audio_tee.as_ref(),
            peer_id,
        );
    }

    /// Applies a viewer's SDP answer; their queued candidates are flushed once
    /// it takes effect.
    pub fn accept_answer(&self, peer_id: &str, sdp: &str) -> Result<(), BroadcastError> {
        let message = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes())
            .map_err(|_| BroadcastError::InvalidSdp)?;
        let negotiated = negotiated_video_codec(&message, &self.shared.codecs);
        let answer =
            gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Answer, message);
        let viewers = self.shared.viewers.lock().expect("viewer lock");
        let Some(entry) = viewers.get(peer_id) else {
            return Ok(());
        };
        let webrtc = entry.webrtc.clone();
        let current = entry.codec;
        drop(viewers);

        // Honour the answer's codec pick before media starts. The offer
        // advertised every ranked codec; a pick other than the pre-built
        // top branch swaps the encode chain on the same connection, and the
        // transceiver preferences narrow to the pick so ICE-restart re-offers
        // cannot flip the codec mid-stream.
        match negotiated {
            None => tracing::warn!(
                peer = peer_id,
                "the answer accepted no offered video codec; this viewer gets audio and chat only"
            ),
            Some((pt, codec)) => {
                if codec != current {
                    tracing::info!(peer = peer_id, offered = ?current, picked = ?codec, "the answer picked another codec");
                }
                self.apply_negotiated_codec(peer_id, pt, codec);
                narrow_codec_preferences(&webrtc, codec, pt);
            }
        }
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
            if let Some((codec, encoder)) = &*entry.rate_target.lock().expect("rate target lock") {
                codec.set_rate(encoder, bitrate_kbps, bitrate_kbps);
            }
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
        let viewers = self.shared.viewers.lock().expect("viewer lock");
        for entry in viewers.values() {
            if let Some(video_valve) = &entry.video_valve {
                video_valve.set_property("drop", paused);
            }
            if let Some(audio_valve) = &entry.audio_valve {
                audio_valve.set_property("drop", paused);
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

/// Removes a viewer's entry and dismantles its branch on a background
/// thread, shared by [`Broadcast::remove_viewer`] and the bus watch's error
/// containment; `true` when the viewer was present. The branch moves into
/// `dismantling` under the same viewer lock that drops the entry, so an
/// error the dying connection posts mid-removal is always attributable to
/// one of the two.
fn remove_viewer_branch(
    shared: &Arc<Shared>,
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    audio_tee: Option<&gst::Element>,
    peer_id: &str,
) -> bool {
    shared
        .display_names
        .lock()
        .expect("display name lock")
        .remove(peer_id);
    let entry = {
        let mut viewers = shared.viewers.lock().expect("viewer lock");
        let Some(entry) = viewers.remove(peer_id) else {
            return false;
        };
        shared
            .dismantling
            .lock()
            .expect("dismantling lock")
            .extend(entry.branch.iter().cloned());
        entry
    };
    let shared = Arc::clone(shared);
    let pipeline = pipeline.clone();
    let tee = tee.clone();
    let audio_tee = audio_tee.cloned();
    crate::teardown::spawn_teardown("clarity-media-viewer-teardown", move || {
        settle_ice_gathering(&entry.webrtc, Instant::now() + Duration::from_secs(5));
        dismantle_viewer(&shared, &pipeline, &tee, audio_tee.as_ref(), &entry);
    });
    true
}

/// Unlinks a removed viewer's tee pads, each under its own idle probe so
/// neither streaming thread ever pushes into a half-removed branch; the last
/// probe to run dismantles the elements.
fn dismantle_viewer(
    shared: &Arc<Shared>,
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    audio_tee: Option<&gst::Element>,
    entry: &ViewerEntry,
) {
    let shared = Arc::clone(shared);
    let pipeline = pipeline.clone();
    let tee = tee.clone();
    let audio_tee = audio_tee.cloned();
    let branch = entry.branch.clone();
    let audio_tee_pad = entry.audio_tee_pad.clone();
    // Detach the audio tap once the video tap is off (or immediately for a
    // viewer whose answer never attached a video branch), then dismantle.
    let after_video = move || {
        match (&audio_tee, &audio_tee_pad) {
            (Some(audio_tee), Some(audio_pad)) => {
                let shared = Arc::clone(&shared);
                let pipeline = pipeline.clone();
                let branch = branch.clone();
                let audio_tee = audio_tee.clone();
                audio_pad.add_probe(gst::PadProbeType::IDLE, move |pad, _| {
                    if let Some(peer) = pad.peer() {
                        let _ = pad.unlink(&peer);
                    }
                    audio_tee.release_request_pad(pad);
                    finish_dismantle(&shared, &pipeline, &branch);
                    gst::PadProbeReturn::Remove
                });
            }
            _ => finish_dismantle(&shared, &pipeline, &branch),
        }
    };
    match &entry.tee_pad {
        Some(tee_pad) => {
            tee_pad.add_probe(gst::PadProbeType::IDLE, move |pad, _| {
                if let Some(peer) = pad.peer() {
                    let _ = pad.unlink(&peer);
                }
                tee.release_request_pad(pad);
                after_video();
                gst::PadProbeReturn::Remove
            });
        }
        None => after_video(),
    }
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
    frame_rate: i32,
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
                // Kept on every re-pin: the constant rate is what keeps the
                // encoder refining a static screen (see the videorate above).
                .field("framerate", gst::Fraction::new(frame_rate, 1))
                .build(),
        );
    });
    Ok(())
}

/// What part of the broadcast a bus error originated in, found by walking the
/// source element's parents. The SCTP and DTLS internals that fail when a
/// viewer's browser dies live inside that viewer's `webrtcbin`, so the walk
/// climbs until it reaches a registered branch element (the bin itself, or a
/// branch element outside it matches directly).
enum ErrorScope {
    /// A live viewer's connection; the viewer fails alone.
    Viewer(String),
    /// A branch already being dismantled; the collapse keeps posting until
    /// teardown finishes, and there is nothing left to do.
    Dismantling,
    /// The shared pipeline; fatal for the broadcast.
    Shared,
}

fn error_scope(shared: &Shared, source: Option<&gst::Object>) -> ErrorScope {
    let mut current = source.cloned();
    while let Some(object) = current {
        if let Some(element) = object.downcast_ref::<gst::Element>() {
            let owner = shared
                .viewers
                .lock()
                .expect("viewer lock")
                .iter()
                .find_map(|(peer_id, entry)| {
                    entry.branch.contains(element).then(|| peer_id.clone())
                });
            if let Some(peer_id) = owner {
                return ErrorScope::Viewer(peer_id);
            }
            if shared
                .dismantling
                .lock()
                .expect("dismantling lock")
                .contains(element)
            {
                return ErrorScope::Dismantling;
            }
        }
        current = object.parent();
    }
    ErrorScope::Shared
}

fn spawn_bus_thread(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    audio_tee: Option<&gst::Element>,
    shared: &Arc<Shared>,
    shutdown: &Arc<AtomicBool>,
) -> Result<JoinHandle<()>, BroadcastError> {
    let bus = pipeline
        .bus()
        .ok_or_else(|| BroadcastError::Start("the pipeline has no message bus".into()))?;
    let pipeline = pipeline.clone();
    let tee = tee.clone();
    let audio_tee = audio_tee.cloned();
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
                            // An error from inside a removable audio leg (an
                            // application stream dying mid-share) downgrades
                            // that leg; the next reconcile removes it. Only
                            // errors from the pipeline proper end the share.
                            let from_leg = message.src().is_some_and(|src| {
                                shared
                                    .audio_leg_names
                                    .lock()
                                    .expect("leg name lock")
                                    .contains(src.name().as_str())
                            });
                            if from_leg {
                                tracing::warn!(
                                    error = %error.error(),
                                    "an audio stream failed; its audio drops from the mix"
                                );
                                continue;
                            }
                            // A failure inside one viewer's connection (its
                            // SCTP association collapsing when the browser
                            // tab closes, a branch element giving up) fails
                            // that viewer alone: the branch is dismantled and
                            // the failure surfaced as the viewer's connection
                            // state, which the session layer's recovery
                            // already handles. Only errors no viewer owns end
                            // the share for everyone.
                            match error_scope(&shared, message.src()) {
                                ErrorScope::Viewer(peer_id) => {
                                    tracing::warn!(
                                        peer = %peer_id,
                                        error = %error.error(),
                                        "a viewer's connection failed; the share continues without it"
                                    );
                                    if remove_viewer_branch(
                                        &shared,
                                        &pipeline,
                                        &tee,
                                        audio_tee.as_ref(),
                                        &peer_id,
                                    ) {
                                        let _ =
                                            shared.events.send(BroadcastEvent::ViewerConnection {
                                                peer_id,
                                                state: ConnectionState::Failed,
                                            });
                                    }
                                    continue;
                                }
                                ErrorScope::Dismantling => {
                                    tracing::debug!(
                                        error = %error.error(),
                                        "a dismantling viewer branch posted an error; ignoring it"
                                    );
                                    continue;
                                }
                                ErrorScope::Shared => {}
                            }
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

/// The multi-codec caps the video transceiver advertises: one structure per
/// ranked codec, payload types 96 upward in rank order.
fn offered_caps(codecs: &[VideoCodec]) -> gst::Caps {
    let mut caps = gst::Caps::new_empty();
    {
        let caps = caps.get_mut().expect("caps are not yet shared");
        for (index, codec) in codecs.iter().enumerate() {
            caps.append_structure(codec.rtp_structure(96 + index as u32));
        }
    }
    caps
}

/// Which offered codec a viewer's answer accepted: the highest-ranked codec
/// whose payload type appears in the answer's video m-line — rank order is
/// the presenter's, the answer only removes what it cannot decode. `None`
/// when the m-line was rejected or names nothing that was offered. Payload
/// types are the authority (RFC 3264 requires the answer to reuse the
/// offer's), double-checked against the encoding name when the answer
/// carries an rtpmap.
fn negotiated_video_codec(
    answer: &gst_sdp::SDPMessage,
    codecs: &[VideoCodec],
) -> Option<(u32, VideoCodec)> {
    let media = answer.medias().find(|media| media.media() == Some("video"))?;
    if media.port() == 0 {
        return None;
    }
    let accepted: Vec<u32> = media
        .formats()
        .filter_map(|format| format.parse().ok())
        .collect();
    for (index, codec) in codecs.iter().enumerate() {
        let pt = 96 + index as u32;
        if !accepted.contains(&pt) {
            continue;
        }
        let name_matches = media
            .caps_from_media(i32::try_from(pt).unwrap_or(96))
            .and_then(|caps| {
                let structure = caps.structure(0)?;
                let name = structure.get::<&str>("encoding-name").ok()?;
                Some(name.eq_ignore_ascii_case(codec.encoding_name()))
            })
            .unwrap_or(true);
        if name_matches {
            return Some((pt, *codec));
        }
    }
    None
}

/// Narrows the video transceiver's preferences to the negotiated codec so
/// later re-offers (ICE restarts, source changes) keep it stable.
fn narrow_codec_preferences(webrtc: &gst::Element, codec: VideoCodec, pt: u32) {
    if let Some(transceiver) = webrtc
        .emit_by_name::<Option<gst_webrtc::WebRTCRTPTransceiver>>("get-transceiver", &[&0i32])
    {
        let mut caps = gst::Caps::new_empty();
        caps.get_mut()
            .expect("caps are not yet shared")
            .append_structure(codec.rtp_structure(pt));
        transceiver.set_property("codec-preferences", caps);
    }
}

/// Attaches a GCC bandwidth estimator to this viewer's connection through
/// webrtcbin's aux-sender hook. The transport-wide estimate has the fixed
/// audio budget subtracted so congestion throttles video and never audio,
/// then drives the encoder one of two ways. The vendored `claritygccbwe`
/// detects the application-limited region itself and holds its estimate
/// through idle periods, so its estimate is applied directly with VBR peak
/// headroom. The stock `rtpgccbwe` fallback collapses its estimate while
/// application limited, so it goes through the adaptive controller, fed a
/// send-rate reading from the video byte counter (video only, so no audio
/// correction). See [`crate::rate`].
fn wire_gcc_bwe(
    webrtc: &gst::Element,
    rate_target: RateTarget,
    ceiling_kbps: u32,
    has_audio: bool,
    target_kbps: Arc<AtomicU32>,
    video_bytes_sent: Arc<AtomicU64>,
) {
    let audio_bps = if has_audio { AUDIO_BITRATE_BPS } else { 0 };
    let min_bps = audio_bps + VIDEO_MIN_KBPS * 1000;
    let max_bps = audio_bps + ceiling_kbps.saturating_mul(1000);
    let start_bps = audio_bps + start_video_kbps(ceiling_kbps) * 1000;
    webrtc.connect("request-aux-sender", false, move |_| {
        // Prefer the vendored element; fall back to the system plugin for
        // GStreamer installs this crate hasn't registered against (e.g. a
        // test harness that skips `ensure_gstreamer`).
        let bwe = gst::ElementFactory::make("claritygccbwe")
            .build()
            .or_else(|_| gst::ElementFactory::make("rtpgccbwe").build());
        let Ok(bwe) = bwe else {
            return None;
        };
        // The bounds are only settable before the element starts; so is the
        // starting estimate on the stock element (the vendored one accepts
        // live writes, unused here).
        bwe.set_property("min-bitrate", min_bps);
        bwe.set_property("max-bitrate", max_bps);
        bwe.set_property("estimated-bitrate", start_bps);
        // The property doubles as the marker for which element was built:
        // only the vendored one has it, and only the vendored one manages
        // the application-limited region itself.
        let vendored = bwe.find_property("pacing-factor").is_some();
        if vendored {
            bwe.set_property("pacing-factor", 2.5f64);
        }
        let rate_target = Arc::clone(&rate_target);
        let target_kbps = Arc::clone(&target_kbps);
        let video_bytes_sent = Arc::clone(&video_bytes_sent);
        let controller = std::sync::Mutex::new(AdaptiveController::new(
            VIDEO_MIN_KBPS,
            ceiling_kbps,
            start_video_kbps(ceiling_kbps),
        ));
        let sampler = std::sync::Mutex::new(SendRateSampler::new());
        bwe.connect_notify(Some("estimated-bitrate"), move |bwe, _| {
            let estimate = bwe.property::<u32>("estimated-bitrate");
            let estimate_kbps = estimate.saturating_sub(audio_bps).max(VIDEO_MIN_KBPS * 1000) / 1000;
            let command = if vendored {
                // The element holds its estimate through application-limited
                // periods and re-measures on exit, so the estimate is current
                // by construction; second-guessing it here would read its
                // legitimate post-idle settle-down as congestion and back
                // off on top of it.
                rate::trust_estimate(estimate_kbps, VIDEO_MIN_KBPS, ceiling_kbps)
            } else {
                // The stock element's estimate collapses while application
                // limited; the controller compensates, fed a send-rate
                // reading fresh enough to catch busy/static transitions.
                // The byte counter covers video only, so unlike the
                // transport-wide estimate no audio budget is subtracted.
                let actual_kbps = sampler
                    .lock()
                    .expect("send rate lock")
                    .sample(video_bytes_sent.load(Ordering::Relaxed), Instant::now());
                controller
                    .lock()
                    .expect("rate lock")
                    .on_estimate(estimate_kbps, actual_kbps)
            };
            // Indirect so the answer-driven codec pick (and any later swap)
            // redirects rate control to the encoder actually in use; before
            // the pick there is nothing to drive.
            if let Some((codec, encoder)) = &*rate_target.lock().expect("rate target lock") {
                codec.set_rate(encoder, command.target_kbps, command.max_kbps);
            }
            target_kbps.store(command.target_kbps, Ordering::Relaxed);
            tracing::debug!(
                estimate_kbps,
                target_kbps = command.target_kbps,
                vendored,
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

/// How many dismantled elements stay registered for error attribution after
/// leaving the pipeline. An error a dying connection posted can still sit in
/// the bus queue when its branch is removed; its by-then-unparented source
/// would read as a shared failure and end the broadcast. Generous next to the
/// ~15 elements a branch holds and the 10-viewer room cap, so an entry only
/// falls out long after its bus messages have drained.
const DISMANTLED_HISTORY: usize = 512;

/// Dismantles a removed viewer's branch. The branch stays in `dismantling`
/// for error attribution (see [`DISMANTLED_HISTORY`]); the registry is
/// pruned oldest-first instead of dropping the branch immediately.
fn finish_dismantle(shared: &Shared, pipeline: &gst::Pipeline, branch: &[gst::Element]) {
    dismantle_branch(pipeline, branch);
    let mut dismantling = shared.dismantling.lock().expect("dismantling lock");
    let excess = dismantling.len().saturating_sub(DISMANTLED_HISTORY);
    if excess > 0 {
        dismantling.drain(..excess);
    }
}

/// Builds the shared audio path up to its tee: what the presenter hears (the
/// default output's monitor) for a real capture, a quiet tone for the
/// synthetic source. Normalized to the one format the Opus encoders consume
/// so per-viewer branches do no conversion work.
/// The audio side of the pipeline: the fixed tail's tee feeding per-viewer
/// legs, plus the swappable head and the tail element it links into, so
/// [`Broadcast::replace_audio_head`] can rebuild the head when the video
/// source changes.
struct AudioChain {
    tee: gst::Element,
    head: Vec<gst::Element>,
    tail_input: gst::Element,
    /// The reconcilable mixer state when the head is the per-stream mix.
    mix: Option<AudioMixState>,
}

fn build_audio_chain(
    pipeline: &gst::Pipeline,
    audio: &AudioCapture,
    mode: AudioHeadMode,
) -> Result<Option<AudioChain>, String> {
    if matches!(audio, AudioCapture::Disabled) {
        return Ok(None);
    }
    for element in ["opusenc", "rtpopuspay"] {
        if gst::ElementFactory::find(element).is_none() {
            return Err(format!("the audio component `{element}` is unavailable"));
        }
    }
    // Head elements whose last member's src pad carries the captured audio: a
    // single source directly, or the reconcilable mixer skeleton for
    // per-application streams.
    let built = build_audio_head(pipeline, audio, mode)?;
    let head = built.elements;
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
    head.last()
        .ok_or("the audio head is empty")?
        .link(&tail[0])
        .map_err(|error| error.to_string())?;
    Ok(Some(AudioChain {
        tee,
        head,
        tail_input: tail[0].clone(),
        mix: built.mix,
    }))
}

/// What building an audio head produced: every element added (output last),
/// plus the mixer state when the head is the reconcilable per-stream mix.
struct BuiltAudioHead {
    elements: Vec<gst::Element>,
    mix: Option<AudioMixState>,
}

impl BuiltAudioHead {
    fn plain(elements: Vec<gst::Element>) -> Self {
        Self {
            elements,
            mix: None,
        }
    }
}

/// The live per-stream mix: a permanent silent leg keeps the mixer producing
/// regardless of how many application legs exist, and each tapped stream is
/// one removable leg keyed by its PipeWire target.
struct AudioMixState {
    mixer: gst::Element,
    legs: std::collections::HashMap<String, Vec<gst::Element>>,
}

fn make_audio(name: &'static str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| format!("the audio component `{name}` is unavailable"))
}

/// Builds the head for the given mode and adds its elements to the pipeline,
/// returning them with the output element last. While idle every variant is
/// a silent placeholder: no audio device or application stream is tapped at
/// all when nothing is being shared.
/// The one audio format every mixer input is normalized to; `audiomixer`
/// requires identical caps on all of its sink pads.
fn mix_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("rate", 48_000)
        .field("channels", 2)
        .build()
}

fn build_audio_head(
    pipeline: &gst::Pipeline,
    audio: &AudioCapture,
    mode: AudioHeadMode,
) -> Result<BuiltAudioHead, String> {
    if mode == AudioHeadMode::Idle {
        let source = gst::ElementFactory::make("audiotestsrc")
            .property("is-live", true)
            .property_from_str("wave", "silence")
            .build()
            .map_err(|error| error.to_string())?;
        pipeline.add(&source).map_err(|error| error.to_string())?;
        return Ok(BuiltAudioHead::plain(vec![source]));
    }
    match audio {
        AudioCapture::Disabled => unreachable!("handled by the caller"),
        AudioCapture::SystemMix if mode == AudioHeadMode::Capture => {
            // "@DEFAULT_MONITOR@" resolves server-side to the monitor of the
            // current default output, following device switches.
            let source = gst::ElementFactory::make("pulsesrc")
                .property("device", "@DEFAULT_MONITOR@")
                .build()
                .map_err(|error| error.to_string())?;
            pipeline.add(&source).map_err(|error| error.to_string())?;
            Ok(BuiltAudioHead::plain(vec![source]))
        }
        AudioCapture::SystemMix => {
            // The synthetic source's soft development tone: audible proof the
            // audio path works without tapping a real device.
            let source = gst::ElementFactory::make("audiotestsrc")
                .property("is-live", true)
                .property("volume", 0.05_f64)
                .property("freq", 440.0_f64)
                .build()
                .map_err(|error| error.to_string())?;
            pipeline.add(&source).map_err(|error| error.to_string())?;
            Ok(BuiltAudioHead::plain(vec![source]))
        }
        AudioCapture::Streams { targets } => build_stream_mix(pipeline, targets),
    }
}

/// The reconcilable per-stream mix: silence + mixer as a permanent skeleton
/// (the mixer always produces, however many legs exist), one leg per target.
/// A leg that cannot be built is skipped with a warning — audio downgrades,
/// it never fails the share — and [`Broadcast::set_audio_streams`] adds and
/// removes legs while streaming.
fn build_stream_mix(
    pipeline: &gst::Pipeline,
    targets: &[String],
) -> Result<BuiltAudioHead, String> {
    let mixer = gst::ElementFactory::make("audiomixer")
        .build()
        .map_err(|_| "the audio component `audiomixer` is unavailable".to_owned())?;
    pipeline.add(&mixer).map_err(|error| error.to_string())?;
    let silence_source = gst::ElementFactory::make("audiotestsrc")
        .property("is-live", true)
        .property_from_str("wave", "silence")
        .build()
        .map_err(|error| error.to_string())?;
    let silence_caps = gst::ElementFactory::make("capsfilter")
        .property("caps", mix_caps())
        .build()
        .map_err(|error| error.to_string())?;
    let silence = [silence_source, silence_caps];
    pipeline
        .add_many(&silence)
        .map_err(|error| error.to_string())?;
    gst::Element::link_many(&silence).map_err(|error| error.to_string())?;
    silence[1].link(&mixer).map_err(|error| error.to_string())?;

    let mut elements: Vec<gst::Element> = silence.to_vec();
    let mut legs = std::collections::HashMap::new();
    for target in targets {
        match build_stream_leg(pipeline, &mixer, target) {
            Ok(leg) => {
                elements.extend(leg.iter().cloned());
                legs.insert(target.clone(), leg);
            }
            Err(reason) => {
                tracing::warn!(%target, %reason, "an audio stream could not be tapped; skipping it");
            }
        }
    }
    // The mixer is the head's output and must stay last.
    elements.push(mixer.clone());
    Ok(BuiltAudioHead {
        elements,
        mix: Some(AudioMixState { mixer, legs }),
    })
}

/// One application leg, linked into the mixer and playing: pipewiresrc for
/// the target, normalized to the mix format.
fn build_stream_leg(
    pipeline: &gst::Pipeline,
    mixer: &gst::Element,
    target: &str,
) -> Result<Vec<gst::Element>, String> {
    let source = application_source(target)?;
    let convert = make_audio("audioconvert")?;
    let resample = make_audio("audioresample")?;
    let caps = gst::ElementFactory::make("capsfilter")
        .property("caps", mix_caps())
        .build()
        .map_err(|error| error.to_string())?;
    let leg = vec![source, convert, resample, caps];
    pipeline.add_many(&leg).map_err(|error| error.to_string())?;
    gst::Element::link_many(&leg).map_err(|error| error.to_string())?;
    let result = (|| {
        leg[leg.len() - 1]
            .link(mixer)
            .map_err(|error| error.to_string())?;
        for element in leg.iter().rev() {
            element
                .sync_state_with_parent()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    })();
    if let Err(reason) = result {
        for element in leg.iter().rev() {
            let _ = element.set_state(gst::State::Null);
        }
        let _ = pipeline.remove_many(&leg);
        return Err(reason);
    }
    Ok(leg)
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
    use super::*;
    use crate::playback::{Playback, PlaybackConfig, PlaybackEvent};
    use clarity_protocol::IceConfiguration;

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

    const LIVE: &str = "viewer-live";
    const DEAD: &str = "viewer-dead";

    fn empty_ice() -> IceConfiguration {
        IceConfiguration {
            expires_at: "2026-01-01T00:00:00Z".into(),
            ice_servers: vec![],
        }
    }

    fn synthetic_config() -> BroadcastConfig {
        BroadcastConfig {
            source: SourceConfig::Synthetic(SyntheticSource {
                width: 320,
                height: 240,
                frame_rate: 15,
            }),
            audio: AudioCapture::Disabled,
            video_codecs: vec![VideoCodecId::Vp8],
            frame_rate: 30,
            ice: empty_ice(),
            force_relay: false,
            preview_frames: None,
            capture_ceiling: None,
        }
    }

    /// Renders media to null sinks so the test runs without a display.
    // The crate denies unsafe code for its own logic; setting the test
    // environment is the one exception, matching the integration suite.
    #[allow(unsafe_code)]
    fn headless() {
        // SAFETY: the other tests in this binary never read the environment.
        unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };
    }

    /// The bug this contains: a viewer's browser tab closing kills its SCTP
    /// association, the error reaches the bus from inside that viewer's
    /// `webrtcbin`, and the whole broadcast used to end for everyone. Media
    /// flows to a healthy viewer, a second viewer's connection posts a fatal
    /// error from inside its bin, and the broadcast must surface that
    /// viewer's failure while the healthy viewer keeps receiving.
    #[tokio::test]
    async fn an_error_inside_one_viewers_connection_fails_that_viewer_alone() {
        headless();
        let (broadcast, mut events) = match Broadcast::start(synthetic_config()) {
            Ok(started) => started,
            // A machine without the media runtime cannot run this test.
            Err(_) => return,
        };
        let settings = EncoderSettings {
            bitrate_kbps: 500,
            adaptive: false,
        };
        broadcast
            .add_viewer(LIVE, settings)
            .expect("the healthy viewer builds");
        broadcast
            .add_viewer(DEAD, settings)
            .expect("the dying viewer builds");
        let (playback, mut playback_events) = Playback::start(PlaybackConfig {
            frames: None,
            native: None,
            audio_samples: None,
            ice: empty_ice(),
            force_relay: false,
        })
        .expect("playback starts");

        // Negotiate the healthy viewer only; the dying viewer's offers go
        // unanswered, like a browser that joined and then went away.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut media = false;
        while !media {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => panic!("media did not flow"),
                event = events.recv() => match event.expect("broadcast stays alive") {
                    BroadcastEvent::Offer { peer_id, sdp, .. } if peer_id == LIVE => {
                        playback.accept_offer(&sdp).expect("offer applies");
                    }
                    BroadcastEvent::IceCandidate { peer_id, candidate, sdp_m_line_index }
                        if peer_id == LIVE =>
                    {
                        playback.add_remote_candidate(sdp_m_line_index, &candidate);
                    }
                    BroadcastEvent::Ended { reason } => panic!("broadcast ended: {reason}"),
                    _ => {}
                },
                event = playback_events.recv() => match event.expect("playback stays alive") {
                    PlaybackEvent::Answer { sdp } => {
                        broadcast.accept_answer(LIVE, &sdp).expect("answer applies");
                    }
                    PlaybackEvent::IceCandidate { candidate, sdp_m_line_index } => {
                        broadcast.add_remote_candidate(LIVE, sdp_m_line_index, &candidate);
                    }
                    PlaybackEvent::Stats(stats) if stats.bitrate_kbps.unwrap_or(0) > 0 => {
                        media = true;
                    }
                    PlaybackEvent::Ended { reason } => panic!("playback ended: {reason}"),
                    _ => {}
                },
            }
        }

        // Fail the dying viewer from inside its connection bin, where the
        // SCTP association errors when the peer vanishes.
        let source = {
            let viewers = broadcast.shared.viewers.lock().expect("viewer lock");
            let webrtc = viewers
                .get(DEAD)
                .expect("the dying viewer exists")
                .webrtc
                .clone();
            drop(viewers);
            webrtc
                .downcast_ref::<gst::Bin>()
                .and_then(|bin| bin.children().into_iter().next())
                .unwrap_or(webrtc)
        };
        gst::element_error!(
            source,
            gst::ResourceError::Write,
            ("simulated: SCTP association went into error state")
        );

        // The broadcast reports the failure as that viewer's, stays alive,
        // and keeps delivering to the healthy viewer.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut failed = false;
        let mut stats_after_failure = 0;
        while !(failed && stats_after_failure >= 2) {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    panic!(
                        "the failure was not contained (failed: {failed}, stats after: {stats_after_failure})"
                    );
                }
                event = events.recv() => match event.expect("broadcast stays alive") {
                    BroadcastEvent::ViewerConnection {
                        peer_id,
                        state: ConnectionState::Failed,
                    } if peer_id == DEAD => failed = true,
                    BroadcastEvent::Ended { reason } => {
                        panic!("one viewer's death ended the broadcast: {reason}");
                    }
                    _ => {}
                },
                event = playback_events.recv() => match event.expect("playback stays alive") {
                    PlaybackEvent::Stats(stats)
                        if failed && stats.bitrate_kbps.unwrap_or(0) > 0 =>
                    {
                        stats_after_failure += 1;
                    }
                    PlaybackEvent::Ended { reason } => {
                        panic!("the healthy viewer's playback ended: {reason}");
                    }
                    _ => {}
                },
            }
        }
        assert!(
            !broadcast
                .shared
                .viewers
                .lock()
                .expect("viewer lock")
                .contains_key(DEAD),
            "the failed viewer's entry is removed"
        );

        broadcast.close();
        playback.close();
        assert!(
            crate::teardown::drain_teardowns(Duration::from_secs(30)),
            "media teardown completed before process exit"
        );
    }

    /// An error owned by no viewer — here from the shared tee — still ends
    /// the broadcast for everyone, exactly as before.
    #[tokio::test]
    async fn an_error_outside_any_viewer_ends_the_broadcast() {
        headless();
        let (broadcast, mut events) = match Broadcast::start(synthetic_config()) {
            Ok(started) => started,
            Err(_) => return,
        };
        broadcast
            .add_viewer(
                LIVE,
                EncoderSettings {
                    bitrate_kbps: 500,
                    adaptive: false,
                },
            )
            .expect("viewer builds");
        gst::element_error!(
            broadcast.tee,
            gst::ResourceError::Write,
            ("simulated: shared pipeline failure")
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => panic!("the broadcast did not end"),
                event = events.recv() => {
                    if let Some(BroadcastEvent::Ended { .. }) = event {
                        break;
                    }
                }
            }
        }
        broadcast.close();
        assert!(
            crate::teardown::drain_teardowns(Duration::from_secs(30)),
            "media teardown completed before process exit"
        );
    }
}
