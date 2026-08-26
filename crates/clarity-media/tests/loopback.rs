//! Streams a synthetic broadcast to a playback instance in-process,
//! exchanging SDP and ICE directly. Exercises the full native media path —
//! encode, DTLS-SRTP, decode — with no server or browser involved.

use std::time::Duration;

use clarity_media::{
    AudioCapture, Broadcast, BroadcastConfig, BroadcastEvent, ConnectionState, EncoderSettings,
    Playback, PlaybackConfig, PlaybackEvent, SourceConfig, SyntheticSource, VideoCodecId,
};
use clarity_protocol::IceConfiguration;
use tokio::sync::mpsc;

const VIEWER: &str = "viewer-1";

fn empty_ice() -> IceConfiguration {
    IceConfiguration {
        expires_at: "2026-01-01T00:00:00Z".into(),
        ice_servers: vec![],
    }
}

fn synthetic() -> SourceConfig {
    SourceConfig::Synthetic(SyntheticSource {
        width: 320,
        height: 240,
        frame_rate: 15,
    })
}

/// Drives the offer/answer/candidate exchange between a broadcast and one
/// playback until the playback receives flowing media, then returns.
async fn pump_until_media(
    broadcast: &Broadcast,
    broadcast_events: &mut mpsc::UnboundedReceiver<BroadcastEvent>,
    playback: &Playback,
    playback_events: &mut mpsc::UnboundedReceiver<PlaybackEvent>,
    viewer: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => panic!("media did not flow"),
            event = broadcast_events.recv() => match event.expect("broadcast stays alive") {
                BroadcastEvent::Offer { peer_id, sdp, .. } if peer_id == viewer => {
                    playback.accept_offer(&sdp).expect("offer applies");
                }
                BroadcastEvent::IceCandidate { peer_id, candidate, sdp_m_line_index }
                    if peer_id == viewer =>
                {
                    playback.add_remote_candidate(sdp_m_line_index, &candidate);
                }
                BroadcastEvent::Ended { reason } => panic!("broadcast ended: {reason}"),
                _ => {}
            },
            event = playback_events.recv() => match event.expect("playback stays alive") {
                PlaybackEvent::Answer { sdp } => {
                    broadcast.accept_answer(viewer, &sdp).expect("answer applies");
                }
                PlaybackEvent::IceCandidate { candidate, sdp_m_line_index } => {
                    broadcast.add_remote_candidate(viewer, sdp_m_line_index, &candidate);
                }
                PlaybackEvent::Stats(stats) if stats.bitrate_kbps.unwrap_or(0) > 0 => return,
                PlaybackEvent::Ended { reason } => panic!("playback ended: {reason}"),
                _ => {}
            },
        }
    }
}

/// Services the signaling exchange (offers, answers, candidates) while
/// waiting for `done` to report true; panics when the deadline passes or
/// either side ends.
async fn pump_until(
    broadcast: &Broadcast,
    broadcast_events: &mut mpsc::UnboundedReceiver<BroadcastEvent>,
    playback: &Playback,
    playback_events: &mut mpsc::UnboundedReceiver<PlaybackEvent>,
    viewer: &str,
    what: &str,
    mut done: impl FnMut() -> bool,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    while !done() {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => panic!("{what} did not happen within 30s"),
            _ = ticker.tick() => {}
            event = broadcast_events.recv() => match event.expect("broadcast stays alive") {
                BroadcastEvent::Offer { peer_id, sdp, .. } if peer_id == viewer => {
                    playback.accept_offer(&sdp).expect("offer applies");
                }
                BroadcastEvent::IceCandidate { peer_id, candidate, sdp_m_line_index }
                    if peer_id == viewer =>
                {
                    playback.add_remote_candidate(sdp_m_line_index, &candidate);
                }
                BroadcastEvent::Ended { reason } => panic!("broadcast ended: {reason}"),
                _ => {}
            },
            event = playback_events.recv() => match event.expect("playback stays alive") {
                PlaybackEvent::Answer { sdp } => {
                    broadcast.accept_answer(viewer, &sdp).expect("answer applies");
                }
                PlaybackEvent::IceCandidate { candidate, sdp_m_line_index } => {
                    broadcast.add_remote_candidate(viewer, sdp_m_line_index, &candidate);
                }
                PlaybackEvent::Ended { reason } => panic!("playback ended: {reason}"),
                _ => {}
            },
        }
    }
}

/// Waits for the background teardown threads before the test returns: process
/// exit while an NVENC teardown is mid-flight deadlocks inside the NVIDIA
/// driver's exit handlers.
fn drain() {
    assert!(
        clarity_media::drain_teardowns(Duration::from_secs(30)),
        "media teardown completed before process exit"
    );
}

/// Whether the most recent decoded frame in `sink` has the given size.
fn frame_size_is(sink: &clarity_media::FrameSink, width: u32, height: u32) -> bool {
    sink.lock()
        .expect("frame lock")
        .as_ref()
        .is_some_and(|frame| frame.width == width && frame.height == height)
}

/// The `ChatMessage` JSON envelope both engines carry on the `chat` channel.
fn envelope(sender: &str, text: &str) -> String {
    serde_json::to_string(&clarity_protocol::ChatMessage {
        sender: sender.to_owned(),
        text: text.to_owned(),
    })
    .expect("chat messages always serialize")
}

/// Exchanges one chat message in each direction, resending until both arrive.
/// The viewer's payload claims a spoofed sender; the hub must surface it
/// stamped with the server-known name registered for the viewer instead.
async fn exchange_chat(
    broadcast: &Broadcast,
    broadcast_events: &mut mpsc::UnboundedReceiver<BroadcastEvent>,
    playback: &Playback,
    playback_events: &mut mpsc::UnboundedReceiver<PlaybackEvent>,
    host_message: &str,
    viewer_message: &str,
) {
    broadcast.set_viewer_display_name(VIEWER, Some("Ada"));
    let host_payload = envelope("Host", host_message);
    let viewer_payload = envelope("Host", viewer_message);
    let stamped_viewer_payload = envelope("Ada", viewer_message);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut ticker = tokio::time::interval(Duration::from_millis(150));
    let mut host_seen_by_viewer = false;
    let mut viewer_seen_by_host = false;
    while !(host_seen_by_viewer && viewer_seen_by_host) {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                panic!("chat not delivered (host→viewer: {host_seen_by_viewer}, viewer→host: {viewer_seen_by_host})");
            }
            _ = ticker.tick() => {
                broadcast.send_chat(&host_payload);
                playback.send_chat(&viewer_payload);
            }
            event = broadcast_events.recv() => {
                if let Some(BroadcastEvent::Chat { text, .. }) = event
                    && text == stamped_viewer_payload
                {
                    viewer_seen_by_host = true;
                }
            }
            event = playback_events.recv() => {
                if let Some(PlaybackEvent::Chat { text }) = event
                    && text == host_payload
                {
                    host_seen_by_viewer = true;
                }
            }
        }
    }
}

#[tokio::test]
async fn broadcast_streams_to_playback_over_loopback() {
    // Media renders to null sinks so the test runs without a display.
    // SAFETY: set before any threads that read the environment are spawned by
    // the media stack; this test is alone in its process.
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let (broadcast, mut broadcast_events) = match Broadcast::start(BroadcastConfig {
        source: synthetic(),
        audio: AudioCapture::Disabled,
        video_codecs: vec![],
        frame_rate: 30,
        ice: empty_ice(),
        force_relay: false,
        preview_frames: None,
        capture_ceiling: None,
    }) {
        Ok(started) => started,
        // A machine without the media runtime cannot run this test.
        Err(_) => return,
    };
    assert!(!broadcast.has_audio());
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
        audio_samples: None,
        ice: empty_ice(),
        force_relay: false,
    })
    .expect("playback starts");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut viewer_connected = false;
    let mut media_received = false;
    while !(viewer_connected && media_received) {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                panic!("no media within 30s (connected: {viewer_connected})");
            }
            event = broadcast_events.recv() => match event.expect("broadcast stays alive") {
                BroadcastEvent::Offer { sdp, .. } => {
                    playback.accept_offer(&sdp).expect("offer applies");
                }
                BroadcastEvent::IceCandidate { candidate, sdp_m_line_index, .. } => {
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
                PlaybackEvent::ConnectionState(ConnectionState::Connected) => {
                    viewer_connected = true;
                }
                PlaybackEvent::Stats(stats) => {
                    if stats.bitrate_kbps.unwrap_or(0) > 0 {
                        media_received = true;
                    }
                }
                PlaybackEvent::Ended { reason } => panic!("playback ended: {reason}"),
                _ => {}
            },
        }
    }

    broadcast.remove_viewer(VIEWER);
    broadcast.close();
    playback.close();
    drain();
}

/// An adaptive viewer negotiates the TWCC extension its GCC estimator feeds
/// on, carries both audio and video, and delivers flowing media — proving the
/// congestion-controlled path (rtpgccbwe attached via the aux-sender hook)
/// connects and streams. The estimator's ramp curve is rtpgccbwe's own
/// behavior and is not asserted here.
#[tokio::test]
async fn adaptive_broadcast_delivers_audio_and_video() {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let (broadcast, mut broadcast_events) = match Broadcast::start(BroadcastConfig {
        source: synthetic(),
        audio: AudioCapture::SystemMix,
        video_codecs: vec![],
        frame_rate: 30,
        ice: empty_ice(),
        force_relay: false,
        preview_frames: None,
        capture_ceiling: None,
    }) {
        Ok(started) => started,
        Err(_) => return,
    };
    assert!(broadcast.has_audio(), "the synthetic source provides audio");
    broadcast
        .add_viewer(
            VIEWER,
            EncoderSettings {
                bitrate_kbps: 4_000,
                adaptive: true,
            },
        )
        .expect("viewer branch builds");
    let (playback, mut playback_events) = Playback::start(PlaybackConfig {
        frames: None,
        native: None,
        audio_samples: None,
        ice: empty_ice(),
        force_relay: false,
    })
    .expect("playback starts");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut connected = false;
    let mut media = false;
    while !(connected && media) {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                panic!("adaptive media did not flow (connected: {connected})");
            }
            event = broadcast_events.recv() => match event.expect("broadcast stays alive") {
                BroadcastEvent::Offer { sdp, .. } => {
                    assert!(
                        sdp.contains("transport-wide-cc-extensions"),
                        "the offer must negotiate the TWCC extension"
                    );
                    assert!(sdp.contains("m=audio"), "the offer must carry audio");
                    assert!(sdp.contains("OPUS"), "audio must be Opus");
                    playback.accept_offer(&sdp).expect("offer applies");
                }
                BroadcastEvent::IceCandidate { candidate, sdp_m_line_index, .. } => {
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
                PlaybackEvent::ConnectionState(ConnectionState::Connected) => connected = true,
                PlaybackEvent::Stats(stats) => {
                    if stats.bitrate_kbps.unwrap_or(0) > 0 {
                        media = true;
                    }
                }
                PlaybackEvent::Ended { reason } => panic!("playback ended: {reason}"),
                _ => {}
            },
        }
    }

    broadcast.close();
    playback.close();
    drain();
}

/// Forcing AV1 offers AV1 and streams it end-to-end to the native decoder,
/// when the AV1 encode chain is installed. Skipped where it is not (or where
/// the codec falls back), so the test never fails without hardware AV1.
#[tokio::test]
async fn av1_broadcast_streams_to_playback() {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let (broadcast, mut broadcast_events) = match Broadcast::start(BroadcastConfig {
        source: synthetic(),
        audio: AudioCapture::Disabled,
        video_codecs: vec![VideoCodecId::Av1],
        frame_rate: 30,
        ice: empty_ice(),
        force_relay: false,
        preview_frames: None,
        capture_ceiling: None,
    }) {
        Ok(started) => started,
        Err(_) => return,
    };
    broadcast
        .add_viewer(
            VIEWER,
            EncoderSettings {
                bitrate_kbps: 2_000,
                adaptive: false,
            },
        )
        .expect("viewer branch builds");
    let (playback, mut playback_events) = Playback::start(PlaybackConfig {
        frames: None,
        native: None,
        audio_samples: None,
        ice: empty_ice(),
        force_relay: false,
    })
    .expect("playback starts");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut offered_av1 = false;
    let mut media = false;
    while !media {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                if !offered_av1 {
                    return; // no AV1 encoder: not applicable
                }
                panic!("AV1 media did not flow");
            }
            event = broadcast_events.recv() => match event.expect("broadcast stays alive") {
                BroadcastEvent::Offer { sdp, .. } => {
                    if !sdp.contains("AV1") {
                        return; // fell back to another codec; not applicable
                    }
                    offered_av1 = true;
                    playback.accept_offer(&sdp).expect("offer applies");
                }
                BroadcastEvent::IceCandidate { candidate, sdp_m_line_index, .. } => {
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
                PlaybackEvent::Stats(stats) => {
                    if stats.bitrate_kbps.unwrap_or(0) > 0 {
                        media = true;
                    }
                }
                PlaybackEvent::Ended { reason } => panic!("playback ended: {reason}"),
                _ => {}
            },
        }
    }

    broadcast.close();
    playback.close();
    drain();
}

/// A mid-session rebuild — the media-layer effect of the presenter's recovery
/// escalation — re-establishes flowing media: the viewer is removed and re-added
/// on the same broadcast while a fresh playback negotiates the new connection,
/// exactly as the session layer does when an ICE restart fails to recover.
#[tokio::test]
async fn rebuilt_viewer_reconnects_and_flows() {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let (broadcast, mut broadcast_events) = match Broadcast::start(BroadcastConfig {
        source: synthetic(),
        audio: AudioCapture::Disabled,
        video_codecs: vec![],
        frame_rate: 30,
        ice: empty_ice(),
        force_relay: false,
        preview_frames: None,
        capture_ceiling: None,
    }) {
        Ok(started) => started,
        Err(_) => return,
    };
    let settings = EncoderSettings {
        bitrate_kbps: 1_000,
        adaptive: false,
    };
    broadcast
        .add_viewer(VIEWER, settings)
        .expect("viewer builds");
    let (playback, mut playback_events) = Playback::start(PlaybackConfig {
        frames: None,
        native: None,
        audio_samples: None,
        ice: empty_ice(),
        force_relay: false,
    })
    .expect("playback starts");
    pump_until_media(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        VIEWER,
    )
    .await;

    // Rebuild: tear down and re-establish both ends, as the recovery escalation
    // does after an ICE restart fails.
    broadcast.remove_viewer(VIEWER);
    playback.close();
    broadcast
        .add_viewer(VIEWER, settings)
        .expect("viewer rebuilds");
    let (playback, mut playback_events) = Playback::start(PlaybackConfig {
        frames: None,
        native: None,
        audio_samples: None,
        ice: empty_ice(),
        force_relay: false,
    })
    .expect("playback restarts");
    pump_until_media(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        VIEWER,
    )
    .await;

    broadcast.close();
    playback.close();
    drain();
}

#[tokio::test]
async fn decoded_frames_reach_the_frame_sink() {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let (broadcast, mut broadcast_events) = match Broadcast::start(BroadcastConfig {
        source: synthetic(),
        audio: AudioCapture::Disabled,
        video_codecs: vec![VideoCodecId::Vp8],
        frame_rate: 30,
        ice: empty_ice(),
        force_relay: false,
        preview_frames: None,
        capture_ceiling: None,
    }) {
        Ok(started) => started,
        Err(_) => return, // no media runtime on this machine
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

    let sink: clarity_media::FrameSink = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (playback, mut playback_events) = Playback::start(PlaybackConfig {
        frames: Some(sink.clone()),
        native: None,
        audio_samples: None,
        ice: empty_ice(),
        force_relay: false,
    })
    .expect("playback starts");

    pump_until_media(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        VIEWER,
    )
    .await;

    // A decoded frame should land in the sink shortly after media flows.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let frame = loop {
        if let Some(frame) = sink.lock().expect("frame lock").clone() {
            break frame;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("no decoded frame reached the sink");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(frame.width > 0 && frame.height > 0, "frame has dimensions");
    assert_eq!(
        frame.data.len(),
        frame.width as usize * frame.height as usize * 4,
        "RGBA is tightly packed"
    );

    drop(broadcast);
    drop(playback);
    drain();
}

#[tokio::test]
async fn chat_flows_both_ways_over_the_data_channel() {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let (broadcast, mut broadcast_events) = match Broadcast::start(BroadcastConfig {
        source: synthetic(),
        audio: AudioCapture::Disabled,
        video_codecs: vec![VideoCodecId::Vp8],
        frame_rate: 30,
        ice: empty_ice(),
        force_relay: false,
        preview_frames: None,
        capture_ceiling: None,
    }) {
        Ok(started) => started,
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
        audio_samples: None,
        ice: empty_ice(),
        force_relay: false,
    })
    .expect("playback starts");

    pump_until_media(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        VIEWER,
    )
    .await;

    // The data channel opens around the same time as media; resend until each
    // side observes the other's message.
    exchange_chat(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        "hello-from-host",
        "hello-from-viewer",
    )
    .await;

    drop(broadcast);
    drop(playback);
    drain();
}

/// The presenter self-preview: a synthetic broadcast taps its capture into a
/// preview `FrameSink`, and RGBA frames arrive even with no viewer connected.
#[tokio::test]
async fn presenter_preview_frames_reach_the_sink() {
    // SAFETY: set before the media stack spawns any env-reading threads; this
    // test is alone in its process.
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let preview: clarity_media::FrameSink = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (_broadcast, _events) = match Broadcast::start(BroadcastConfig {
        source: synthetic(),
        audio: AudioCapture::Disabled,
        video_codecs: vec![],
        frame_rate: 30,
        ice: empty_ice(),
        force_relay: false,
        preview_frames: Some(preview.clone()),
        capture_ceiling: None,
    }) {
        Ok(started) => started,
        // A machine without the media runtime cannot run this test.
        Err(_) => return,
    };

    // No viewer is added; the tee's preview branch runs on its own.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut frame = None;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        frame = preview.lock().expect("preview lock").clone();
        if frame.is_some() {
            break;
        }
    }

    let frame = frame.expect("the presenter preview received a frame");
    assert!(
        frame.width > 0 && frame.height > 0,
        "preview frame has real dimensions"
    );
    assert_eq!(
        frame.data.len(),
        frame.width as usize * frame.height as usize * 4,
        "preview frame is tightly packed RGBA",
    );

    drop(_broadcast);
    drain();
}

/// A presenter-driven ICE restart renegotiates the transport in place: the
/// broadcast produces an `Offer { ice_restart: true }`, the viewer answers on
/// the same connection, and frames keep arriving afterwards.
#[tokio::test]
async fn ice_restart_keeps_media_flowing() {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let (broadcast, mut broadcast_events) = match Broadcast::start(BroadcastConfig {
        source: synthetic(),
        audio: AudioCapture::Disabled,
        video_codecs: vec![VideoCodecId::Vp8],
        frame_rate: 30,
        ice: empty_ice(),
        force_relay: false,
        preview_frames: None,
        capture_ceiling: None,
    }) {
        Ok(started) => started,
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
        audio_samples: None,
        ice: empty_ice(),
        force_relay: false,
    })
    .expect("playback starts");

    pump_until_media(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        VIEWER,
    )
    .await;

    broadcast.restart_ice(VIEWER);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut restart_offered = false;
    let mut answered = false;
    let mut stats_after_answer = 0;
    while !(restart_offered && stats_after_answer >= 2) {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                panic!("media did not survive the ICE restart (restart offer: {restart_offered}, answered: {answered})");
            }
            event = broadcast_events.recv() => match event.expect("broadcast stays alive") {
                BroadcastEvent::Offer { peer_id, sdp, ice_restart } if peer_id == VIEWER => {
                    if ice_restart {
                        restart_offered = true;
                    }
                    playback.accept_offer(&sdp).expect("restart offer applies");
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
                    broadcast.accept_answer(VIEWER, &sdp).expect("restart answer applies");
                    answered = true;
                }
                PlaybackEvent::IceCandidate { candidate, sdp_m_line_index } => {
                    broadcast.add_remote_candidate(VIEWER, sdp_m_line_index, &candidate);
                }
                PlaybackEvent::Stats(stats)
                    if answered && stats.bitrate_kbps.unwrap_or(0) > 0 =>
                {
                    stats_after_answer += 1;
                }
                PlaybackEvent::Ended { reason } => panic!("playback ended: {reason}"),
                _ => {}
            },
        }
    }

    broadcast.close();
    playback.close();
    drain();
}

/// A mid-stream source swap never touches the viewer's connection: source A
/// is replaced by a differently sized source B, decoded frames arrive at B's
/// dimensions on the same negotiated connection, and chat still flows both
/// ways afterwards.
#[tokio::test]
async fn replace_source_swaps_mid_stream() {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let (broadcast, mut broadcast_events) = match Broadcast::start(BroadcastConfig {
        source: synthetic(),
        audio: AudioCapture::Disabled,
        video_codecs: vec![VideoCodecId::Vp8],
        frame_rate: 30,
        ice: empty_ice(),
        force_relay: false,
        preview_frames: None,
        capture_ceiling: None,
    }) {
        Ok(started) => started,
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
    let sink: clarity_media::FrameSink = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (playback, mut playback_events) = Playback::start(PlaybackConfig {
        frames: Some(sink.clone()),
        native: None,
        audio_samples: None,
        ice: empty_ice(),
        force_relay: false,
    })
    .expect("playback starts");

    pump_until_media(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        VIEWER,
    )
    .await;

    broadcast
        .replace_source(SourceConfig::Synthetic(SyntheticSource {
            width: 480,
            height: 360,
            frame_rate: 15,
        }))
        .expect("the source swaps");

    pump_until(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        VIEWER,
        "frames from the replacement source",
        || frame_size_is(&sink, 480, 360),
    )
    .await;

    exchange_chat(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        "host-after-swap",
        "viewer-after-swap",
    )
    .await;

    broadcast.close();
    playback.close();
    drain();
}

/// `idle()` swaps the capture out for the internal placeholder without
/// dropping the viewer: black idle frames keep arriving on the same
/// connection, and a later `replace_source` resumes real frames on it.
#[tokio::test]
async fn idle_then_replace_source_resumes_on_the_same_connection() {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let (broadcast, mut broadcast_events) = match Broadcast::start(BroadcastConfig {
        source: synthetic(),
        audio: AudioCapture::Disabled,
        video_codecs: vec![VideoCodecId::Vp8],
        frame_rate: 30,
        ice: empty_ice(),
        force_relay: false,
        preview_frames: None,
        capture_ceiling: None,
    }) {
        Ok(started) => started,
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
    let sink: clarity_media::FrameSink = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (playback, mut playback_events) = Playback::start(PlaybackConfig {
        frames: Some(sink.clone()),
        native: None,
        audio_samples: None,
        ice: empty_ice(),
        force_relay: false,
    })
    .expect("playback starts");

    pump_until_media(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        VIEWER,
    )
    .await;

    broadcast.idle().expect("idle swaps in the placeholder");
    // The 640x360 idle placeholder reaches the viewer: the connection is
    // alive with no capture behind it.
    pump_until(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        VIEWER,
        "idle placeholder frames",
        || frame_size_is(&sink, 640, 360),
    )
    .await;

    broadcast
        .replace_source(SourceConfig::Synthetic(SyntheticSource {
            width: 320,
            height: 240,
            frame_rate: 15,
        }))
        .expect("going live again");
    pump_until(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        VIEWER,
        "frames after going live again",
        || frame_size_is(&sink, 320, 240),
    )
    .await;

    broadcast.close();
    playback.close();
    drain();
}

/// The desktop flow that regressed: a room opened idle with system audio kept
/// the silent placeholder head for the whole broadcast, so going live
/// streamed the placeholder (then a test tone) instead of the real monitor.
/// The audio head must follow the source across `replace_source`, and the
/// swap must not disturb media or chat on the negotiated connection.
#[tokio::test]
async fn audio_head_follows_the_source_out_of_idle() {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let (broadcast, mut broadcast_events) = match Broadcast::start(BroadcastConfig {
        source: SourceConfig::Idle,
        audio: AudioCapture::SystemMix,
        video_codecs: vec![VideoCodecId::Vp8],
        frame_rate: 30,
        ice: empty_ice(),
        force_relay: false,
        preview_frames: None,
        capture_ceiling: None,
    }) {
        Ok(started) => started,
        Err(_) => return,
    };
    assert!(
        broadcast.has_audio(),
        "an idle-opened broadcast still negotiates an audio leg"
    );
    broadcast
        .add_viewer(
            VIEWER,
            EncoderSettings {
                bitrate_kbps: 500,
                adaptive: false,
            },
        )
        .expect("viewer branch builds");
    let sink: clarity_media::FrameSink = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (playback, mut playback_events) = Playback::start(PlaybackConfig {
        frames: Some(sink.clone()),
        native: None,
        audio_samples: None,
        ice: empty_ice(),
        force_relay: false,
    })
    .expect("playback starts");

    pump_until(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        VIEWER,
        "idle placeholder frames",
        || frame_size_is(&sink, 640, 360),
    )
    .await;

    // Going live swaps both heads: video to the new source, audio from the
    // idle silence to the mode's capture. The synthetic mode's head is the
    // development tone, standing in for `pulsesrc` which needs an audio
    // server this test cannot assume.
    broadcast
        .replace_source(SourceConfig::Synthetic(SyntheticSource {
            width: 320,
            height: 240,
            frame_rate: 15,
        }))
        .expect("going live");
    pump_until(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        VIEWER,
        "frames after going live",
        || frame_size_is(&sink, 320, 240),
    )
    .await;

    // The audio-head swap must not have disturbed the data channel either.
    exchange_chat(
        &broadcast,
        &mut broadcast_events,
        &playback,
        &mut playback_events,
        "audio-swap-host",
        "audio-swap-viewer",
    )
    .await;

    broadcast.close();
    playback.close();
    drain();
}
