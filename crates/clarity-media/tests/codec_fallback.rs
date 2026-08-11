//! Codec fallback end to end: the offer advertises every ranked codec, the
//! viewer's answer picks the best it can decode, and the broadcast swaps its
//! encoder to match. Lives in its own test binary because the decode-deny
//! hook is process-wide: `CLARITY_DECODE_DENY` makes this process's
//! `Playback` behave like a viewer without those decoders.

use std::time::Duration;

use clarity_media::{
    AudioCapture, Broadcast, BroadcastConfig, BroadcastEvent, EncoderSettings, Playback,
    PlaybackConfig, PlaybackEvent, SourceConfig, SyntheticSource,
};
use clarity_protocol::IceConfiguration;

const VIEWER: &str = "viewer-1";

/// A viewer that cannot decode the two top-ranked codecs negotiates the
/// third: the answer accepts H264, the broadcast swaps its encoder from AV1
/// to H264 on the same connection, and media flows.
#[tokio::test]
async fn falls_back_when_the_viewer_cannot_decode_the_top_codecs() {
    // SAFETY: set before the media stack spawns any env-reading threads;
    // this test is alone in its process.
    unsafe {
        std::env::set_var("CLARITY_MEDIA_HEADLESS", "1");
        std::env::set_var("CLARITY_DECODE_DENY", "AV1,H265");
    }

    let (broadcast, mut broadcast_events) = match Broadcast::start(BroadcastConfig {
        source: SourceConfig::Synthetic(SyntheticSource {
            width: 320,
            height: 240,
            frame_rate: 15,
        }),
        audio: AudioCapture::Disabled,
        video_codecs: vec![],
        frame_rate: 30,
        ice: IceConfiguration {
            expires_at: "2026-01-01T00:00:00Z".into(),
            ice_servers: vec![],
        },
        force_relay: false,
        preview_frames: None,
        capture_ceiling: None,
    }) {
        Ok(started) => started,
        // A machine without the media runtime cannot run this test; neither
        // can one without the hardware encoders the fallback starts from.
        Err(_) => return,
    };
    broadcast
        .add_viewer(
            VIEWER,
            EncoderSettings {
                bitrate_kbps: 500,
                adaptive: false,
            },
        )
        .expect("viewer branch builds");
    let (playback, mut playback_events) = Playback::start(PlaybackConfig {
        frames: None,
        native: None,
        ice: IceConfiguration {
            expires_at: "2026-01-01T00:00:00Z".into(),
            ice_servers: vec![],
        },
        force_relay: false,
    })
    .expect("playback starts");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut negotiated: Option<String> = None;
    loop {
        if let Some(codec) = &negotiated {
            assert_eq!(codec, "H264", "the answer's pick drives the encoder");
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => panic!("fallback media did not flow"),
            event = broadcast_events.recv() => match event.expect("broadcast stays alive") {
                BroadcastEvent::Offer { peer_id, sdp, .. } if peer_id == VIEWER => {
                    // The offer must advertise the whole ranking; the denied
                    // codecs are still offered, only the viewer drops them.
                    assert!(sdp.contains("AV1"), "the offer advertises AV1");
                    assert!(sdp.contains("H264"), "the offer advertises H264");
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
                PlaybackEvent::Stats(stats) if stats.bitrate_kbps.unwrap_or(0) > 0 => {
                    negotiated = stats.codec.clone();
                }
                PlaybackEvent::Ended { reason } => panic!("playback ended: {reason}"),
                _ => {}
            },
        }
    }

    broadcast.close();
    playback.close();
    assert!(
        clarity_media::drain_teardowns(Duration::from_secs(30)),
        "media teardown completed before process exit"
    );
}
