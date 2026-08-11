//! Negotiates a real answer from `Playback` against a Chrome-style offer and
//! verifies the feedback mechanisms the presenter's bandwidth estimator needs
//! survive into it. Without transport-cc and the TWCC header extension in the
//! answer, browsers fall back to their slowest congestion-control ramp.

use std::time::Duration;

use clarity_media::{Playback, PlaybackConfig, PlaybackEvent};
use clarity_protocol::IceConfiguration;

const CHROME_STYLE_OFFER: &str = concat!(
    "v=0\r\n",
    "o=- 123456789 2 IN IP4 127.0.0.1\r\n",
    "s=-\r\n",
    "t=0 0\r\n",
    "a=group:BUNDLE 0 1\r\n",
    "a=msid-semantic: WMS stream\r\n",
    "m=video 9 UDP/TLS/RTP/SAVPF 96 97\r\n",
    "c=IN IP4 0.0.0.0\r\n",
    "a=rtcp:9 IN IP4 0.0.0.0\r\n",
    "a=ice-ufrag:someufrag\r\n",
    "a=ice-pwd:somepasswordsomepassword\r\n",
    "a=ice-options:trickle\r\n",
    "a=fingerprint:sha-256 ",
    "AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:",
    "AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA\r\n",
    "a=setup:actpass\r\n",
    "a=mid:0\r\n",
    "a=extmap:2 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r\n",
    "a=sendonly\r\n",
    "a=msid:stream track-video\r\n",
    "a=rtcp-mux\r\n",
    "a=rtpmap:96 VP8/90000\r\n",
    "a=rtcp-fb:96 transport-cc\r\n",
    "a=rtcp-fb:96 goog-remb\r\n",
    "a=rtcp-fb:96 nack\r\n",
    "a=rtcp-fb:96 nack pli\r\n",
    "a=rtcp-fb:96 ccm fir\r\n",
    "a=rtpmap:97 rtx/90000\r\n",
    "a=fmtp:97 apt=96\r\n",
    "a=ssrc:1111 cname:test\r\n",
    "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
    "c=IN IP4 0.0.0.0\r\n",
    "a=rtcp:9 IN IP4 0.0.0.0\r\n",
    "a=ice-ufrag:someufrag\r\n",
    "a=ice-pwd:somepasswordsomepassword\r\n",
    "a=fingerprint:sha-256 ",
    "AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:",
    "AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA\r\n",
    "a=setup:actpass\r\n",
    "a=mid:1\r\n",
    "a=extmap:2 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r\n",
    "a=sendonly\r\n",
    "a=msid:stream track-audio\r\n",
    "a=rtcp-mux\r\n",
    "a=rtpmap:111 opus/48000/2\r\n",
    "a=rtcp-fb:111 transport-cc\r\n",
    "a=fmtp:111 minptime=10;useinbandfec=1\r\n",
    "a=ssrc:2222 cname:test\r\n",
);

#[tokio::test]
async fn answers_preserve_congestion_control_feedback() {
    let (playback, mut events) = match Playback::start(PlaybackConfig {
        frames: None,
        native: None,
        ice: IceConfiguration {
            expires_at: "2026-01-01T00:00:00Z".into(),
            ice_servers: vec![],
        },
        force_relay: false,
    }) {
        Ok(started) => started,
        // A machine without the media runtime cannot run this test.
        Err(_) => return,
    };
    playback
        .accept_offer(CHROME_STYLE_OFFER)
        .expect("offer parses");

    let answer = loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("negotiation completes in time")
            .expect("playback stays alive");
        match event {
            PlaybackEvent::Answer { sdp } => break sdp,
            PlaybackEvent::Ended { reason } => panic!("playback ended: {reason}"),
            _ => {}
        }
    };
    playback.close();

    println!("--- answer ---\n{answer}\n---");
    let video: Vec<&str> = answer
        .split("m=")
        .find(|section| section.starts_with("video"))
        .expect("answer has a video section")
        .lines()
        .collect();
    let has = |needle: &str| video.iter().any(|line| line.contains(needle));

    assert!(has("transport-cc"), "TWCC feedback missing from the answer");
    assert!(
        has("transport-wide-cc-extensions"),
        "TWCC header extension missing from the answer"
    );
    assert!(
        has("nack pli"),
        "PLI keyframe recovery missing from the answer"
    );
    assert!(has("a=recvonly"), "viewer answer must be receive-only");
    assert!(
        clarity_media::drain_teardowns(Duration::from_secs(30)),
        "media teardown completed before process exit"
    );
}
