//! Resolves a user-supplied application name to the PipeWire audio stream to
//! capture. Only applications currently playing audio have a stream node, so
//! resolution is a snapshot of "what is audible right now".

use std::process::Command;

/// Voice-chat applications kept out of a shared mix unless the presenter
/// asks for them, so an ongoing call is never broadcast by accident. The
/// default exclusion set for both the CLI and the desktop GUI.
pub const DEFAULT_EXCLUDED: &[&str] = &["discord"];

/// [`DEFAULT_EXCLUDED`] as the owned list the policy functions take.
pub fn default_excluded() -> Vec<String> {
    DEFAULT_EXCLUDED
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

/// One application playback stream currently live on the audio graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioApplication {
    /// PipeWire object serial, usable as a capture target.
    pub serial: u64,
    /// The application's advertised name or stream name.
    pub label: String,
    /// The application's process binary, when known.
    pub binary: Option<String>,
}

impl AudioApplication {
    /// Name to show a person, including the binary when it adds information
    /// (stream names like "WEBRTC VoiceEngine" rarely identify the app).
    pub fn display_name(&self) -> String {
        match &self.binary {
            Some(binary) if !self.label.eq_ignore_ascii_case(binary) => {
                format!("{} ({binary})", self.label)
            }
            _ => self.label.clone(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AudioAppError {
    #[error("the audio graph could not be inspected (is PipeWire running?): {0}")]
    Unavailable(String),
    #[error("no application matching \"{name}\" is playing audio right now{}", available_hint(.available))]
    NotFound {
        name: String,
        available: Vec<String>,
    },
    #[error("\"{name}\" matches several applications: {}; be more specific", .matches.join(", "))]
    Ambiguous { name: String, matches: Vec<String> },
}

fn available_hint(available: &[String]) -> String {
    if available.is_empty() {
        "; nothing is playing audio".to_owned()
    } else {
        format!("; currently playing: {}", available.join(", "))
    }
}

/// Lists the applications currently playing audio, one entry per
/// application even when it has several streams.
pub fn playing_applications() -> Result<Vec<AudioApplication>, AudioAppError> {
    Ok(applications_in_dump(&graph_snapshot()?))
}

/// Finds the application whose name, binary, or stream name contains `name`
/// (case-insensitive). Several streams from the same application resolve to
/// its first stream; matches across different applications are ambiguous.
pub fn resolve_application(name: &str) -> Result<AudioApplication, AudioAppError> {
    resolve_in_dump(&graph_snapshot()?, name)
}

fn graph_snapshot() -> Result<serde_json::Value, AudioAppError> {
    let output = Command::new("pw-dump")
        .output()
        .map_err(|error| AudioAppError::Unavailable(error.to_string()))?;
    if !output.status.success() {
        return Err(AudioAppError::Unavailable(format!(
            "pw-dump exited with {}",
            output.status
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| AudioAppError::Unavailable(error.to_string()))
}

fn applications_in_dump(dump: &serde_json::Value) -> Vec<AudioApplication> {
    let mut applications: Vec<AudioApplication> = Vec::new();
    for stream in playback_streams(dump) {
        let application = stream.application();
        if !applications
            .iter()
            .any(|known| known.display_name() == application.display_name())
        {
            applications.push(application);
        }
    }
    applications
}

/// The applications currently playing audio that none of `excluded` matches,
/// for sharing the system's audio with those applications removed. Matching
/// is the same case-insensitive contains test as [`resolve_application`]. An
/// exclusion that matches nothing playing is an error, so a typo does not
/// silently share the very audio the presenter meant to keep private.
pub fn applications_except(excluded: &[String]) -> Result<Vec<AudioApplication>, AudioAppError> {
    applications_except_in(&graph_snapshot()?, excluded, MissingExclusion::Reject)
}

/// How [`applications_except_in`] treats an exclusion that matches no playing
/// stream: `Reject` for names a person typed (a typo must not silently share
/// the audio it was meant to remove), `Ignore` for built-in default
/// exclusions (an idle voice app excludes nothing, and that is fine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingExclusion {
    Reject,
    Ignore,
}

fn applications_except_in(
    dump: &serde_json::Value,
    excluded: &[String],
    missing: MissingExclusion,
) -> Result<Vec<AudioApplication>, AudioAppError> {
    let streams = playback_streams(dump);
    if missing == MissingExclusion::Reject {
        for name in excluded {
            let needle = name.to_lowercase();
            let matches = streams.iter().any(|stream| {
                stream
                    .match_terms()
                    .any(|term| term.to_lowercase().contains(&needle))
            });
            if !matches {
                let mut available: Vec<String> = streams
                    .iter()
                    .map(|stream| stream.application().display_name())
                    .collect();
                available.sort();
                available.dedup();
                return Err(AudioAppError::NotFound {
                    name: name.clone(),
                    available,
                });
            }
        }
    }
    let kept: Vec<AudioApplication> = streams
        .iter()
        .filter(|stream| {
            !excluded.iter().any(|name| {
                let needle = name.to_lowercase();
                stream
                    .match_terms()
                    .any(|term| term.to_lowercase().contains(&needle))
            })
        })
        .map(|stream| stream.application())
        .collect();
    Ok(kept)
}

/// The audio decision for a share with no explicit audio choice, built so an
/// excluded application can never be broadcast by accident:
///
/// - What is playing (minus exclusions) is captured per stream, never as the
///   device monitor — an excluded app that starts playing mid-share is
///   structurally unreachable, because only the snapshotted streams are
///   tapped.
/// - When nothing shareable is playing, or the graph cannot be read, the
///   share carries no audio at all. The old behaviour fell back to the full
///   monitor mix here, which is exactly how an idle-at-start voice app ended
///   up audible the moment a call began.
///
/// The monitor mix is only ever an explicit choice (`--all-audio`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultAudioDecision {
    /// Capture exactly these applications' streams.
    Share(Vec<AudioApplication>),
    /// Share without audio; the reason drives the presenter-facing warning.
    NoAudio(NoAudioReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoAudioReason {
    /// Only excluded applications are playing.
    OnlyExcludedPlaying,
    /// Nothing is playing audio at all.
    NothingPlaying,
    /// The audio graph could not be inspected, so exclusions cannot be
    /// honoured; sharing the monitor anyway could leak the excluded app.
    GraphUnreadable(String),
}

/// Decides the default-share audio against the live graph. See
/// [`DefaultAudioDecision`] for the policy.
pub fn default_audio_exclusion(excluded: &[String]) -> DefaultAudioDecision {
    match graph_snapshot() {
        Ok(dump) => decide_default_audio(&dump, excluded),
        Err(error) => {
            DefaultAudioDecision::NoAudio(NoAudioReason::GraphUnreadable(error.to_string()))
        }
    }
}

/// One watchdog poll: the stream targets the shared mix should hold right
/// now under the exclusion policy. `None` when the graph is unreadable, so a
/// transient `pw-dump` failure leaves the current mix alone instead of
/// silencing it.
pub fn excluded_mix_targets(excluded: &[String]) -> Option<Vec<String>> {
    match default_audio_exclusion(excluded) {
        DefaultAudioDecision::Share(kept) => {
            Some(kept.iter().map(|app| app.serial.to_string()).collect())
        }
        DefaultAudioDecision::NoAudio(NoAudioReason::GraphUnreadable(_)) => None,
        DefaultAudioDecision::NoAudio(_) => Some(Vec::new()),
    }
}

fn decide_default_audio(dump: &serde_json::Value, excluded: &[String]) -> DefaultAudioDecision {
    let all = playback_streams(dump);
    let kept = applications_except_in(dump, excluded, MissingExclusion::Ignore)
        .expect("Ignore never rejects");
    if !kept.is_empty() {
        DefaultAudioDecision::Share(kept)
    } else if all.is_empty() {
        DefaultAudioDecision::NoAudio(NoAudioReason::NothingPlaying)
    } else {
        DefaultAudioDecision::NoAudio(NoAudioReason::OnlyExcludedPlaying)
    }
}

fn resolve_in_dump(
    dump: &serde_json::Value,
    name: &str,
) -> Result<AudioApplication, AudioAppError> {
    let streams = playback_streams(dump);
    let needle = name.to_lowercase();
    let matches: Vec<&PlaybackStream> = streams
        .iter()
        .filter(|stream| {
            stream
                .match_terms()
                .any(|term| term.to_lowercase().contains(&needle))
        })
        .collect();
    let mut labels: Vec<String> = matches
        .iter()
        .map(|stream| stream.application().display_name())
        .collect();
    labels.dedup();
    match (matches.first(), labels.len()) {
        (Some(stream), 1) => Ok(stream.application()),
        (Some(_), _) => Err(AudioAppError::Ambiguous {
            name: name.to_owned(),
            matches: labels,
        }),
        (None, _) => {
            let mut available: Vec<String> = streams
                .iter()
                .map(|stream| stream.application().display_name())
                .collect();
            available.sort();
            available.dedup();
            Err(AudioAppError::NotFound {
                name: name.to_owned(),
                available,
            })
        }
    }
}

struct PlaybackStream {
    serial: u64,
    application_name: Option<String>,
    binary: Option<String>,
    node_name: Option<String>,
}

impl PlaybackStream {
    fn match_terms(&self) -> impl Iterator<Item = &str> {
        [
            self.application_name.as_deref(),
            self.binary.as_deref(),
            self.node_name.as_deref(),
        ]
        .into_iter()
        .flatten()
    }

    fn application(&self) -> AudioApplication {
        AudioApplication {
            serial: self.serial,
            label: self
                .application_name
                .as_deref()
                .or(self.node_name.as_deref())
                .unwrap_or("unnamed")
                .to_owned(),
            binary: self.binary.clone(),
        }
    }
}

fn playback_streams(dump: &serde_json::Value) -> Vec<PlaybackStream> {
    let Some(objects) = dump.as_array() else {
        return Vec::new();
    };
    objects
        .iter()
        .filter(|object| object["type"] == "PipeWire:Interface:Node")
        .filter_map(|object| {
            let props = &object["info"]["props"];
            if props["media.class"] != "Stream/Output/Audio" {
                return None;
            }
            Some(PlaybackStream {
                serial: props["object.serial"].as_u64()?,
                application_name: props["application.name"].as_str().map(str::to_owned),
                binary: props["application.process.binary"]
                    .as_str()
                    .map(str::to_owned),
                node_name: props["node.name"].as_str().map(str::to_owned),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump() -> serde_json::Value {
        serde_json::json!([
            {
                "type": "PipeWire:Interface:Node",
                "info": { "props": {
                    "media.class": "Stream/Output/Audio",
                    "object.serial": 22689,
                    "node.name": "WEBRTC VoiceEngine",
                    "application.name": "WEBRTC VoiceEngine",
                    "application.process.binary": "Discord",
                    "media.name": "playStream"
                }}
            },
            {
                "type": "PipeWire:Interface:Node",
                "info": { "props": {
                    "media.class": "Stream/Output/Audio",
                    "object.serial": 30001,
                    "node.name": "spotify",
                    "application.name": "Spotify",
                    "application.process.binary": "spotify"
                }}
            },
            {
                "type": "PipeWire:Interface:Node",
                "info": { "props": {
                    "media.class": "Stream/Input/Audio",
                    "object.serial": 40001,
                    "application.name": "Some Recorder"
                }}
            },
            { "type": "PipeWire:Interface:Port" }
        ])
    }

    #[test]
    fn resolves_by_binary_name_case_insensitively() {
        let resolved = resolve_in_dump(&dump(), "discord").expect("resolves");
        assert_eq!(resolved.serial, 22689);
    }

    #[test]
    fn resolves_by_application_name() {
        let resolved = resolve_in_dump(&dump(), "spot").expect("resolves");
        assert_eq!(resolved.serial, 30001);
        assert_eq!(resolved.label, "Spotify");
    }

    #[test]
    fn recording_streams_are_not_candidates() {
        assert!(matches!(
            resolve_in_dump(&dump(), "recorder"),
            Err(AudioAppError::NotFound { .. })
        ));
    }

    #[test]
    fn unknown_names_list_what_is_playing() {
        let Err(AudioAppError::NotFound { available, .. }) = resolve_in_dump(&dump(), "vlc") else {
            panic!("expected NotFound");
        };
        assert_eq!(available, ["Spotify", "WEBRTC VoiceEngine (Discord)"]);
    }

    #[test]
    fn lists_one_entry_per_application_with_recognizable_names() {
        let applications = applications_in_dump(&dump());
        let names: Vec<String> = applications
            .iter()
            .map(AudioApplication::display_name)
            .collect();
        assert_eq!(names, ["WEBRTC VoiceEngine (Discord)", "Spotify"]);
    }

    #[test]
    fn matches_across_different_applications_are_ambiguous() {
        // "s" hits both Spotify and the Discord voice engine stream.
        assert!(matches!(
            resolve_in_dump(&dump(), "s"),
            Err(AudioAppError::Ambiguous { .. })
        ));
    }

    /// The graph while the voice app is idle: Discord runs but plays nothing,
    /// so it has no output stream — only the music does.
    fn dump_without_discord_playing() -> serde_json::Value {
        serde_json::json!([
            {
                "type": "PipeWire:Interface:Node",
                "info": { "props": {
                    "media.class": "Stream/Output/Audio",
                    "object.serial": 30001,
                    "node.name": "spotify",
                    "application.name": "Spotify",
                    "application.process.binary": "spotify"
                }}
            }
        ])
    }

    #[test]
    fn excluding_an_app_keeps_the_rest() {
        let kept =
            applications_except_in(&dump(), &["discord".to_owned()], MissingExclusion::Reject)
                .expect("resolves");
        let names: Vec<String> = kept.iter().map(AudioApplication::display_name).collect();
        assert_eq!(names, ["Spotify"]);
    }

    #[test]
    fn excluding_something_not_playing_is_an_error_when_strict() {
        // Guards against a typo silently sharing the audio meant to be private.
        assert!(matches!(
            applications_except_in(&dump(), &["discrod".to_owned()], MissingExclusion::Reject),
            Err(AudioAppError::NotFound { .. })
        ));
    }

    #[test]
    fn lenient_exclusion_tolerates_an_idle_app() {
        let kept = applications_except_in(
            &dump_without_discord_playing(),
            &["discord".to_owned()],
            MissingExclusion::Ignore,
        )
        .expect("never rejects");
        let names: Vec<String> = kept.iter().map(AudioApplication::display_name).collect();
        assert_eq!(names, ["Spotify"]);
    }

    #[test]
    fn default_excludes_a_playing_voice_app() {
        let decision = decide_default_audio(&dump(), &["discord".to_owned()]);
        let DefaultAudioDecision::Share(kept) = decision else {
            panic!("music keeps the share audible");
        };
        let names: Vec<String> = kept.iter().map(AudioApplication::display_name).collect();
        assert_eq!(names, ["Spotify"]);
    }

    /// The reported leak: Discord idle at share start used to fall back to
    /// the full monitor mix, so a call starting mid-share was broadcast. The
    /// default must capture the playing streams individually instead — a
    /// later-starting excluded app is then structurally unreachable.
    #[test]
    fn default_never_falls_back_to_the_monitor_when_the_voice_app_is_idle() {
        let decision =
            decide_default_audio(&dump_without_discord_playing(), &["discord".to_owned()]);
        let DefaultAudioDecision::Share(kept) = decision else {
            panic!("playing music is shared per stream");
        };
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].label, "Spotify");
    }

    #[test]
    fn default_shares_no_audio_when_only_the_voice_app_plays() {
        let voice_only = serde_json::json!([
            {
                "type": "PipeWire:Interface:Node",
                "info": { "props": {
                    "media.class": "Stream/Output/Audio",
                    "object.serial": 22689,
                    "node.name": "WEBRTC VoiceEngine",
                    "application.name": "WEBRTC VoiceEngine",
                    "application.process.binary": "Discord"
                }}
            }
        ]);
        assert_eq!(
            decide_default_audio(&voice_only, &["discord".to_owned()]),
            DefaultAudioDecision::NoAudio(NoAudioReason::OnlyExcludedPlaying)
        );
    }

    #[test]
    fn default_shares_no_audio_when_nothing_plays() {
        assert_eq!(
            decide_default_audio(&serde_json::json!([]), &["discord".to_owned()]),
            DefaultAudioDecision::NoAudio(NoAudioReason::NothingPlaying)
        );
    }
}
