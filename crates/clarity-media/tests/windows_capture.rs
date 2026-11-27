//! Windows capture end to end: a real display or window captured through
//! Windows Graphics Capture with WASAPI loopback audio, encoded, sent over an
//! in-process WebRTC connection, and decoded. Windows only, and it needs an
//! interactive desktop session with at least one display.
#![cfg(windows)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use clarity_media::{
    AudioCapture, AudioSampleSink, Broadcast, BroadcastConfig, BroadcastEvent, CaptureRequest,
    CaptureSource, CaptureStream, CaptureTarget, EncoderSettings, FrameSink, Playback,
    PlaybackConfig, PlaybackEvent, SourceConfig, VideoCodecId, VideoFrame, capture_sources,
};
use clarity_protocol::IceConfiguration;

const VIEWER: &str = "viewer-1";
/// The capture ceiling the tests broadcast at; any real display is larger.
const CEILING: (u32, u32) = (1280, 720);

fn empty_ice() -> IceConfiguration {
    IceConfiguration {
        expires_at: "2026-01-01T00:00:00Z".into(),
        ice_servers: vec![],
    }
}

/// The picker's first display — the primary one — streams to a viewer as
/// real frames, scaled to the capture ceiling, with system audio alongside.
#[tokio::test]
async fn the_primary_display_streams_with_system_audio() {
    let display = capture_sources()
        .into_iter()
        .find(|source| matches!(source.target, CaptureTarget::Display(_)))
        .expect("the desktop has a display");

    let frame = stream_one_frame(&display).await;

    // A display larger than the ceiling is scaled to fit it exactly along
    // one axis, aspect preserved.
    assert!(
        frame.width <= CEILING.0 && frame.height <= CEILING.1,
        "{}x{} exceeds the ceiling",
        frame.width,
        frame.height
    );
    assert!(
        frame.width == CEILING.0 || frame.height == CEILING.1,
        "{}x{} was not fitted to the ceiling ({})",
        frame.width,
        frame.height,
        display.detail
    );
}

/// A single window streams through the window-handle path. Skipped when no
/// shareable, unminimized window is open.
#[tokio::test]
async fn a_window_streams_through_window_capture() {
    let Some(window) = capture_sources().into_iter().find(|source| {
        matches!(source.target, CaptureTarget::Window(_)) && !source.detail.ends_with("minimized")
    }) else {
        eprintln!("no unminimized window to share; skipping");
        return;
    };

    let frame = stream_one_frame(&window).await;

    assert!(
        frame.width >= 2 && frame.height >= 2,
        "{}x{} from {:?}",
        frame.width,
        frame.height,
        window.name
    );
}

/// Captures `source` with system audio, connects one in-process viewer, and
/// returns the first decoded frame once both video and audio have arrived.
async fn stream_one_frame(source: &CaptureSource) -> VideoFrame {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let capture = CaptureStream::open(CaptureRequest {
        show_cursor: true,
        target: Some(source.target),
        ..Default::default()
    })
    .await
    .expect("the source opens for capture");

    let (broadcast, mut broadcast_events) = Broadcast::start(BroadcastConfig {
        source: SourceConfig::Screen(capture),
        audio: AudioCapture::SystemMix,
        video_codecs: vec![VideoCodecId::Vp8],
        frame_rate: 30,
        ice: empty_ice(),
        force_relay: false,
        preview_frames: None,
        capture_ceiling: Some(CEILING),
    })
    .expect("a real capture broadcast starts");
    assert!(
        broadcast.has_audio(),
        "system audio negotiates an audio leg"
    );
    broadcast
        .add_viewer(
            VIEWER,
            EncoderSettings {
                bitrate_kbps: 4_000,
                adaptive: false,
            },
        )
        .expect("viewer branch builds");

    let frames: FrameSink = Arc::new(Mutex::new(None));
    let samples: AudioSampleSink = Arc::new(Mutex::new(Vec::new()));
    let (playback, mut playback_events) = Playback::start(PlaybackConfig {
        frames: Some(frames.clone()),
        native: None,
        audio_samples: Some(samples.clone()),
        ice: empty_ice(),
        force_relay: false,
    })
    .expect("playback starts");

    let received = || {
        frames.lock().expect("frame lock").is_some()
            && !samples.lock().expect("sample lock").is_empty()
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    while !received() {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => panic!(
                "media from {:?} did not arrive (video: {}, audio samples: {})",
                source.name,
                frames.lock().expect("frame lock").is_some(),
                samples.lock().expect("sample lock").len(),
            ),
            _ = ticker.tick() => {}
            event = broadcast_events.recv() => match event.expect("broadcast stays alive") {
                BroadcastEvent::Offer { peer_id, sdp, .. } if peer_id == VIEWER => {
                    playback.accept_offer(&sdp).expect("offer applies");
                }
                BroadcastEvent::IceCandidate { peer_id, candidate, sdp_m_line_index }
                    if peer_id == VIEWER =>
                {
                    playback.add_remote_candidate(sdp_m_line_index, &candidate);
                }
                BroadcastEvent::Ended { reason } => panic!("broadcast ended: {reason}"),
                _ => {}
            },
            event = playback_events.recv() => match event.expect("playback stays alive") {
                PlaybackEvent::Answer { sdp } => {
                    broadcast.accept_answer(VIEWER, &sdp).expect("answer applies");
                }
                PlaybackEvent::IceCandidate { candidate, sdp_m_line_index } => {
                    broadcast.add_remote_candidate(VIEWER, sdp_m_line_index, &candidate);
                }
                PlaybackEvent::Ended { reason } => panic!("playback ended: {reason}"),
                _ => {}
            },
        }
    }

    let frame = frames
        .lock()
        .expect("frame lock")
        .clone()
        .expect("a frame arrived");
    drop(playback);
    drop(broadcast);
    assert!(
        clarity_media::drain_teardowns(Duration::from_secs(30)),
        "media teardown completed before process exit"
    );
    frame
}
