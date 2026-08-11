//! Media engine for the native Clarity Share client.
//!
//! [`Playback`] receives one presenter's WebRTC stream, decodes it, and
//! renders it into a dedicated video window with accompanying audio.
//! [`Broadcast`] sends one video source to any number of viewers, each on an
//! independent connection with its own encoder. Callers exchange only Clarity
//! domain values: SDP text, ICE candidates, and
//! [`clarity_protocol::IceConfiguration`]. The underlying media stack is not
//! part of the interface and may change without affecting callers.

// `deny` rather than `forbid`: the native video overlay must speak to
// libwayland and the GstVideoOverlay C API directly, so `overlay.rs` opts back
// in with a module-level allow. Everything else stays safe code.
#![deny(unsafe_code)]

mod broadcast;
mod capture;
mod ice;
mod overlay;
mod playback;
mod rate;
mod stats;
mod teardown;

pub use broadcast::{
    AudioCapture, Broadcast, BroadcastConfig, BroadcastError, BroadcastEvent, EncoderSettings,
    SourceConfig, SyntheticSource, VideoCodecCapability, VideoCodecId, video_codec_inventory,
};
pub use capture::{CaptureError, CaptureRequest, CaptureStream};
pub use overlay::{NativeHandle, NativeVideoSurface};
pub use playback::{
    ConnectionState, FrameSink, IceState, Playback, PlaybackConfig, PlaybackError, PlaybackEvent,
    VideoFrame,
};
pub use stats::{SenderStats, StreamStats};
pub use teardown::drain_teardowns;
