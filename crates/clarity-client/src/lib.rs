//! Native client for Clarity Share.
//!
//! Mirrors the web client's session architecture: a signaling client owns the
//! WebSocket lifecycle (authentication, resume, heartbeats, reconnection), and
//! role-specific sessions drive media through `clarity-media` while speaking
//! only `clarity-protocol` types to the server.

#![forbid(unsafe_code)]

pub mod audio_apps;
pub mod invite;
pub mod presence;
pub mod presenter;
pub mod rooms;
pub mod signaling;
pub mod viewer;

/// Media types that appear in this crate's public session APIs, re-exported so
/// callers need not depend on `clarity-media` directly.
pub use clarity_media::{
    AudioCapture, CaptureError, CaptureRequest, CaptureStream, ConnectionState, FrameSink,
    NativeHandle, NativeVideoSurface, SenderStats, SourceConfig, StreamStats, SyntheticSource,
    VideoCodecCapability, VideoCodecId, VideoFrame, video_codec_inventory,
};
