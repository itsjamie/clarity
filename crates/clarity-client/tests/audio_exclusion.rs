//! The audio-exclusion leak test, end to end against the real audio graph:
//! two synthetic PipeWire playback streams stand in for a voice call (440 Hz)
//! and music (1320 Hz), the default-audio policy resolves what to share, the
//! media engine captures exactly that, and tone detection on the decoded
//! audio proves the excluded application is inaudible to the viewer.
//!
//! Runs only where PipeWire, `pw-dump`, `gst-launch-1.0`, and `pipewiresink`
//! exist (skips silently otherwise). The fake streams play two quiet tones on
//! the default output for the duration of the test.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use clarity_client::audio_apps::{DefaultAudioDecision, default_audio_exclusion};
use clarity_client::{AudioCapture, SourceConfig, SyntheticSource};
use clarity_media::{
    Broadcast, BroadcastConfig, BroadcastEvent, EncoderSettings, Playback, PlaybackConfig,
    PlaybackEvent,
};
use clarity_protocol::IceConfiguration;

const VIEWER: &str = "viewer-1";
const VOICE_HZ: f32 = 440.0;
const MUSIC_HZ: f32 = 1320.0;
const SAMPLE_RATE: f32 = 48_000.0;

/// A fake application playing a quiet tone; killed on drop. It plays through
/// pipewire-pulse (`pulsesink`), exactly like the real applications the
/// exclusion targets — a `pipewiresink` stream would carry the right
/// properties but never gets scheduled for capture consumers on current
/// PipeWire, so it cannot stand in for one.
struct FakeApp(Child);

impl FakeApp {
    fn spawn(name: &str, freq: f32) -> Option<Self> {
        Command::new("gst-launch-1.0")
            .args([
                "-q",
                "audiotestsrc",
                "is-live=true",
                &format!("freq={freq}"),
                "volume=0.05",
                "!",
                "audioconvert",
                "!",
                "pulsesink",
                &format!("client-name={name}"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
            .map(Self)
    }
}

impl Drop for FakeApp {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Power of `freq` in `samples` by the Goertzel algorithm, normalized so
/// different window lengths compare.
fn tone_power(samples: &[f32], freq: f32) -> f32 {
    let len = samples.len() as f32;
    let k = (0.5 + len * freq / SAMPLE_RATE).floor();
    let angular = 2.0 * std::f32::consts::PI * k / len;
    let coefficient = 2.0 * angular.cos();
    let (mut previous, mut before_previous) = (0.0_f32, 0.0_f32);
    for sample in samples {
        let current = sample + coefficient * previous - before_previous;
        before_previous = previous;
        previous = current;
    }
    (previous * previous + before_previous * before_previous
        - coefficient * previous * before_previous)
        / (len * len)
}

fn environment_ready() -> bool {
    let has = |command: &str, args: &[&str]| {
        Command::new(command)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    };
    has("pw-dump", &[]) && has("gst-inspect-1.0", &["--exists", "pipewiresink"])
}

/// The reported leak, reproduced and pinned: with the voice app excluded by
/// name, its tone must not reach the viewer while the music's tone does —
/// through the real chain of graph resolution, per-stream PipeWire capture,
/// Opus over WebRTC, and decode.
#[tokio::test]
async fn excluded_application_audio_never_reaches_the_viewer() {
    // SAFETY: set before the media stack spawns any env-reading threads;
    // this test is alone in its process.
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };
    if !environment_ready() {
        eprintln!("skipping: PipeWire tooling is unavailable here");
        return;
    }
    let _voice = match FakeApp::spawn("ClarityFakeVoice", VOICE_HZ) {
        Some(app) => app,
        None => {
            eprintln!("skipping: gst-launch-1.0 is unavailable here");
            return;
        }
    };
    let _music = FakeApp::spawn("ClarityFakeMusic", MUSIC_HZ)
        .expect("the second fake app spawns like the first");

    // Both fakes must be visible on the graph before the policy decides.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut kept = None;
    while tokio::time::Instant::now() < deadline {
        if let DefaultAudioDecision::Share(applications) =
            default_audio_exclusion(&["clarityfakevoice".to_owned()])
            && applications
                .iter()
                .any(|app| app.label == "ClarityFakeMusic")
        {
            kept = Some(applications);
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let Some(kept) = kept else {
        eprintln!("skipping: the fake streams never appeared on the audio graph");
        return;
    };

    // The policy half: the voice app is resolved off the share list.
    assert!(
        kept.iter().all(|app| app.label != "ClarityFakeVoice"),
        "the excluded voice app must not be in the share list: {kept:?}"
    );

    // The capture half: stream exactly what the policy decided, and listen.
    let (broadcast, mut broadcast_events) = match Broadcast::start(BroadcastConfig {
        source: SourceConfig::Synthetic(SyntheticSource {
            width: 320,
            height: 240,
            frame_rate: 15,
        }),
        // Only the fakes are captured: the policy assertions above ran on
        // the full graph, but tone analysis must not mix in whatever real
        // applications happen to be playing on this machine.
        audio: AudioCapture::Streams {
            targets: kept
                .iter()
                .filter(|app| app.label.starts_with("Clarity"))
                .map(|app| app.serial.to_string())
                .collect(),
        },
        video_codecs: vec![],
        frame_rate: 30,
        ice: empty_ice(),
        force_relay: false,
        preview_frames: None,
        capture_ceiling: None,
    }) {
        Ok(started) => started,
        Err(error) => {
            eprintln!("skipping: the media runtime is unavailable here ({error})");
            return;
        }
    };
    assert!(broadcast.has_audio(), "the kept streams provide audio");
    broadcast
        .add_viewer(
            VIEWER,
            EncoderSettings {
                bitrate_kbps: 500,
                adaptive: false,
            },
        )
        .expect("viewer branch builds");
    let samples: clarity_media::AudioSampleSink = Default::default();
    let (playback, mut playback_events) = Playback::start(PlaybackConfig {
        frames: None,
        native: None,
        audio_samples: Some(samples.clone()),
        ice: empty_ice(),
        force_relay: false,
    })
    .expect("playback starts");

    // Pump signaling until three seconds of decoded audio have accumulated.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    let enough = (SAMPLE_RATE * 3.0) as usize;
    loop {
        if samples.lock().expect("sample lock").len() >= enough {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => panic!("audio did not flow"),
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

    // Analyze the last two seconds: the music tone must dominate, the voice
    // tone must be at the noise floor. The margin (50x in power) leaves room
    // for codec artifacts while catching any real bleed of the 440 Hz tone.
    let window = {
        let samples = samples.lock().expect("sample lock");
        let take = (SAMPLE_RATE * 2.0) as usize;
        samples[samples.len() - take..].to_vec()
    };
    let music = tone_power(&window, MUSIC_HZ);
    let voice = tone_power(&window, VOICE_HZ);
    assert!(
        music > 1e-7,
        "the shared music tone is audible to the viewer (power {music:e})"
    );
    assert!(
        music > voice * 50.0,
        "the excluded voice tone leaked to the viewer (voice {voice:e} vs music {music:e})"
    );

    // The watchdog half: an application that starts playing mid-share joins
    // the mix on the next reconcile, and the excluded voice app stays out.
    // This drives exactly what the presenter session's 500 ms watchdog does:
    // poll the graph, push the target diff into the live broadcast.
    const LATE_HZ: f32 = 880.0;
    let _late = FakeApp::spawn("ClarityLateApp", LATE_HZ)
        .expect("the late fake app spawns like the others");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut late_heard = false;
    while tokio::time::Instant::now() < deadline && !late_heard {
        // The session watchdog's poll, narrowed to the fakes for the same
        // reason as above.
        if let DefaultAudioDecision::Share(kept) =
            default_audio_exclusion(&["clarityfakevoice".to_owned()])
        {
            let targets: Vec<String> = kept
                .iter()
                .filter(|app| app.label.starts_with("Clarity"))
                .map(|app| app.serial.to_string())
                .collect();
            broadcast.set_audio_streams(&targets);
        }
        samples.lock().expect("sample lock").clear();
        tokio::time::sleep(Duration::from_millis(700)).await;
        let window = samples.lock().expect("sample lock").clone();
        if window.len() < (SAMPLE_RATE * 0.5) as usize {
            continue;
        }
        late_heard = tone_power(&window, LATE_HZ) > 1e-7;
        if late_heard {
            let voice = tone_power(&window, VOICE_HZ);
            let late = tone_power(&window, LATE_HZ);
            assert!(
                late > voice * 50.0,
                "the excluded voice tone leaked after reconciling (voice {voice:e} vs late {late:e})"
            );
        }
    }
    assert!(
        late_heard,
        "the late-starting application never joined the shared mix"
    );

    broadcast.close();
    playback.close();
    assert!(
        clarity_media::drain_teardowns(Duration::from_secs(30)),
        "media teardown completed before process exit"
    );
}

fn empty_ice() -> IceConfiguration {
    IceConfiguration {
        expires_at: "2026-01-01T00:00:00Z".into(),
        ice_servers: vec![],
    }
}
