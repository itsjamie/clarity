//! Media engine for the native Clarity Share client.
//!
//! [`Playback`] receives one presenter's WebRTC stream, decodes it, and
//! renders it into a dedicated video window with accompanying audio.
//! [`Broadcast`] sends one video source to any number of viewers, each on an
//! independent connection with its own encoder. Callers exchange only Clarity
//! domain values: SDP text, ICE candidates, and
//! [`clarity_protocol::IceConfiguration`]. The underlying media stack is not
//! part of the interface and may change without affecting callers.
//!
//! Screen capture is platform-specific. On Linux the desktop portal's picker
//! chooses the source and PipeWire streams it. Windows has no system picker
//! for desktop applications, so callers list sources with
//! `capture_sources` and pass the chosen `CaptureTarget` in the
//! [`CaptureRequest`]; capture runs on Windows Graphics Capture.

// `deny` rather than `forbid`: the native video overlay must speak to
// libwayland and the GstVideoOverlay C API directly, and Windows source and
// audio-session enumeration call Win32 and COM, so those modules opt back in
// with a module-level allow. Everything else stays safe code.
#![deny(unsafe_code)]

#[cfg(windows)]
#[path = "audio_sessions_windows.rs"]
mod audio_sessions;
mod broadcast;
#[cfg(not(windows))]
mod capture;
#[cfg(windows)]
#[path = "capture_windows.rs"]
mod capture;
pub(crate) mod gcc;
mod ice;
#[cfg(not(windows))]
mod overlay;
#[cfg(windows)]
#[path = "overlay_windows.rs"]
mod overlay;
mod playback;
mod rate;
mod stats;
mod teardown;

#[cfg(windows)]
pub use audio_sessions::{AudioSession, ProcessInfo, audio_sessions, running_processes};
pub use broadcast::{
    AudioCapture, Broadcast, BroadcastConfig, BroadcastError, BroadcastEvent, EncoderSettings,
    SourceConfig, SyntheticSource, VideoCodecCapability, VideoCodecId, video_codec_inventory,
};
pub use capture::{CaptureError, CaptureRequest, CaptureStream};
#[cfg(windows)]
pub use capture::{CaptureSource, CaptureTarget, capture_sources};
pub use overlay::{NativeHandle, NativeVideoSurface};
pub use playback::{
    AudioSampleSink, ConnectionState, FrameSink, IceState, Playback, PlaybackConfig, PlaybackError,
    PlaybackEvent, VideoFrame,
};
pub use stats::{SenderStats, StreamStats};
pub use teardown::drain_teardowns;
