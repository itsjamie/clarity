//! Resolves a user-supplied application name to the PipeWire audio stream to
//! capture. Only applications currently playing audio have a stream node, so
//! resolution is a snapshot of "what is audible right now".

use std::process::Command;

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
    let dump = graph_snapshot()?;
    let streams = playback_streams(&dump);
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

    fn applications_except_in_dump(
        dump: &serde_json::Value,
        excluded: &[String],
    ) -> Result<Vec<AudioApplication>, AudioAppError> {
        // Mirrors applications_except without the live pw-dump call.
        let streams = playback_streams(dump);
        for name in excluded {
            let needle = name.to_lowercase();
            if !streams.iter().any(|stream| {
                stream
                    .match_terms()
                    .any(|term| term.to_lowercase().contains(&needle))
            }) {
                return Err(AudioAppError::NotFound {
                    name: name.clone(),
                    available: vec![],
                });
            }
        }
        Ok(streams
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
            .collect())
    }

    #[test]
    fn excluding_an_app_keeps_the_rest() {
        let kept = applications_except_in_dump(&dump(), &["discord".to_owned()]).expect("resolves");
        let names: Vec<String> = kept.iter().map(AudioApplication::display_name).collect();
        assert_eq!(names, ["Spotify"]);
    }

    #[test]
    fn excluding_something_not_playing_is_an_error() {
        // Guards against a typo silently sharing the audio meant to be private.
        assert!(matches!(
            applications_except_in_dump(&dump(), &["discrod".to_owned()]),
            Err(AudioAppError::NotFound { .. })
        ));
    }
}
