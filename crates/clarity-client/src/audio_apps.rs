//! Resolves a user-supplied application name to the audio stream to
//! capture. Only applications currently playing audio have a stream, so
//! resolution is a snapshot of "what is audible right now".
//!
//! On Linux a stream is a PipeWire playback node, captured by its object
//! serial. On Windows it is a process with an audio session, captured with
//! process loopback — which takes the process's whole tree, children
//! included. A tree that contains an excluded application therefore cannot
//! be captured without leaking it, and is excluded too; and a stream whose
//! ancestor is already captured is not captured again (see
//! `windows_streams`).

#[cfg(not(windows))]
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
    /// The capture target: the PipeWire object serial on Linux, the process
    /// id on Windows.
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
    #[cfg_attr(
        not(windows),
        error("the audio graph could not be inspected (is PipeWire running?): {0}")
    )]
    #[cfg_attr(windows, error("the audio sessions could not be inspected: {0}"))]
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
    Ok(applications_in(&snapshot()?))
}

/// Finds the application whose name, binary, or stream name contains `name`
/// (case-insensitive). Several streams from the same application resolve to
/// its first stream; matches across different applications are ambiguous.
pub fn resolve_application(name: &str) -> Result<AudioApplication, AudioAppError> {
    resolve_in(&snapshot()?, name)
}

/// The playback streams live right now.
#[cfg(not(windows))]
fn snapshot() -> Result<Vec<PlaybackStream>, AudioAppError> {
    Ok(playback_streams(&graph_snapshot()?))
}

/// The processes with an audio session right now, with the process-tree
/// relatives the exclusion policy needs.
#[cfg(windows)]
fn snapshot() -> Result<Vec<PlaybackStream>, AudioAppError> {
    let sessions: Vec<(u32, Option<String>)> = clarity_media::audio_sessions()
        .map_err(AudioAppError::Unavailable)?
        .into_iter()
        .map(|session| (session.process_id, session.display_name))
        .collect();
    let processes: Vec<Process> = clarity_media::running_processes()
        .into_iter()
        .map(|process| Process {
            id: process.process_id,
            parent: process.parent_id,
            executable: process.executable,
            created: process.created,
        })
        .collect();
    Ok(windows_streams(&sessions, &processes, std::process::id()))
}

#[cfg(not(windows))]
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

fn applications_in(streams: &[PlaybackStream]) -> Vec<AudioApplication> {
    let mut applications: Vec<AudioApplication> = Vec::new();
    for stream in streams {
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
    applications_except_in(&snapshot()?, excluded, MissingExclusion::Reject)
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
    streams: &[PlaybackStream],
    excluded: &[String],
    missing: MissingExclusion,
) -> Result<Vec<AudioApplication>, AudioAppError> {
    if missing == MissingExclusion::Reject {
        for name in excluded {
            let needle = name.to_lowercase();
            let matches = streams.iter().any(|stream| {
                stream
                    .exclusion_terms()
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
    let kept: Vec<&PlaybackStream> = streams
        .iter()
        .filter(|stream| {
            !excluded.iter().any(|name| {
                let needle = name.to_lowercase();
                stream
                    .exclusion_terms()
                    .any(|term| term.to_lowercase().contains(&needle))
            })
        })
        .collect();
    // A process-tree capture already carries its descendants; capturing a
    // kept descendant on its own as well would play its audio twice.
    // (Linux streams have no ancestors, so nothing is dropped there.)
    let kept_targets: Vec<u64> = kept.iter().map(|stream| stream.serial).collect();
    Ok(kept
        .into_iter()
        .filter(|stream| {
            !stream
                .ancestors
                .iter()
                .any(|ancestor| kept_targets.contains(ancestor))
        })
        .map(PlaybackStream::application)
        .collect())
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
    match snapshot() {
        Ok(streams) => decide_default_audio(&streams, excluded),
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

fn decide_default_audio(streams: &[PlaybackStream], excluded: &[String]) -> DefaultAudioDecision {
    let kept = applications_except_in(streams, excluded, MissingExclusion::Ignore)
        .expect("Ignore never rejects");
    if !kept.is_empty() {
        DefaultAudioDecision::Share(kept)
    } else if streams.is_empty() {
        DefaultAudioDecision::NoAudio(NoAudioReason::NothingPlaying)
    } else {
        DefaultAudioDecision::NoAudio(NoAudioReason::OnlyExcludedPlaying)
    }
}

fn resolve_in(streams: &[PlaybackStream], name: &str) -> Result<AudioApplication, AudioAppError> {
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
    /// Executables running below this stream's process (Windows), all of
    /// which a process-tree capture of it would also hear. Empty on Linux.
    descendants: Vec<String>,
    /// The capture targets of the processes above this one (Windows). Empty
    /// on Linux.
    ancestors: Vec<u64>,
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

    /// What an exclusion is tested against: the stream's own names, plus
    /// every executable its capture would carry along.
    fn exclusion_terms(&self) -> impl Iterator<Item = &str> {
        self.match_terms()
            .chain(self.descendants.iter().map(String::as_str))
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

#[cfg(any(not(windows), test))]
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
                descendants: Vec::new(),
                ancestors: Vec::new(),
            })
        })
        .collect()
}

/// A running process, as the Windows snapshot sees it.
#[cfg(any(windows, test))]
struct Process {
    id: u32,
    parent: u32,
    executable: String,
    /// Start time, when known; see [`windows_streams`].
    created: Option<u64>,
}

/// One stream per process with an audio session, labelled with the session's
/// own name (else the executable), carrying the process-tree relatives the
/// exclusion policy needs. This process and pid 0 are skipped, as is a
/// session whose process has already exited (it cannot be captured).
///
/// Parent links come from the process table, and process ids are recycled:
/// a "parent" that started after its child is a different process that
/// inherited the id, so the walk stops there. Where start times are unknown
/// the link is trusted — that can only exclude more, never leak.
#[cfg(any(windows, test))]
fn windows_streams(
    sessions: &[(u32, Option<String>)],
    processes: &[Process],
    own_process: u32,
) -> Vec<PlaybackStream> {
    let by_id: std::collections::HashMap<u32, &Process> = processes
        .iter()
        .map(|process| (process.id, process))
        .collect();
    let parent_of = |process: &Process| -> Option<&Process> {
        let parent = *by_id.get(&process.parent)?;
        if parent.id == process.id {
            return None;
        }
        match (parent.created, process.created) {
            (Some(parent_started), Some(child_started)) if parent_started > child_started => None,
            _ => Some(parent),
        }
    };
    // Ancestor ids, nearest first; bounded against cycles of recycled ids.
    let ancestors_of = |id: u32| -> Vec<u32> {
        let mut chain = Vec::new();
        let mut current = by_id.get(&id).copied();
        while let Some(process) = current {
            let Some(parent) = parent_of(process) else {
                break;
            };
            if chain.contains(&parent.id) || chain.len() >= 64 {
                break;
            }
            chain.push(parent.id);
            current = Some(parent);
        }
        chain
    };

    sessions
        .iter()
        .filter(|(id, _)| *id != 0 && *id != own_process)
        .filter_map(|(id, display_name)| {
            let process = by_id.get(id)?;
            let descendants = processes
                .iter()
                .filter(|other| other.id != *id && ancestors_of(other.id).contains(id))
                .map(|other| other.executable.clone())
                .collect();
            Some(PlaybackStream {
                serial: u64::from(*id),
                application_name: Some(
                    display_name
                        .clone()
                        .unwrap_or_else(|| process.executable.clone()),
                ),
                binary: Some(process.executable.clone()),
                node_name: None,
                descendants,
                ancestors: ancestors_of(*id).into_iter().map(u64::from).collect(),
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
        let resolved = resolve_in(&playback_streams(&dump()), "discord").expect("resolves");
        assert_eq!(resolved.serial, 22689);
    }

    #[test]
    fn resolves_by_application_name() {
        let resolved = resolve_in(&playback_streams(&dump()), "spot").expect("resolves");
        assert_eq!(resolved.serial, 30001);
        assert_eq!(resolved.label, "Spotify");
    }

    #[test]
    fn recording_streams_are_not_candidates() {
        assert!(matches!(
            resolve_in(&playback_streams(&dump()), "recorder"),
            Err(AudioAppError::NotFound { .. })
        ));
    }

    #[test]
    fn unknown_names_list_what_is_playing() {
        let Err(AudioAppError::NotFound { available, .. }) =
            resolve_in(&playback_streams(&dump()), "vlc")
        else {
            panic!("expected NotFound");
        };
        assert_eq!(available, ["Spotify", "WEBRTC VoiceEngine (Discord)"]);
    }

    #[test]
    fn lists_one_entry_per_application_with_recognizable_names() {
        let applications = applications_in(&playback_streams(&dump()));
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
            resolve_in(&playback_streams(&dump()), "s"),
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
        let kept = applications_except_in(
            &playback_streams(&dump()),
            &["discord".to_owned()],
            MissingExclusion::Reject,
        )
        .expect("resolves");
        let names: Vec<String> = kept.iter().map(AudioApplication::display_name).collect();
        assert_eq!(names, ["Spotify"]);
    }

    #[test]
    fn excluding_something_not_playing_is_an_error_when_strict() {
        // Guards against a typo silently sharing the audio meant to be private.
        assert!(matches!(
            applications_except_in(
                &playback_streams(&dump()),
                &["discrod".to_owned()],
                MissingExclusion::Reject
            ),
            Err(AudioAppError::NotFound { .. })
        ));
    }

    #[test]
    fn lenient_exclusion_tolerates_an_idle_app() {
        let kept = applications_except_in(
            &playback_streams(&dump_without_discord_playing()),
            &["discord".to_owned()],
            MissingExclusion::Ignore,
        )
        .expect("never rejects");
        let names: Vec<String> = kept.iter().map(AudioApplication::display_name).collect();
        assert_eq!(names, ["Spotify"]);
    }

    #[test]
    fn default_excludes_a_playing_voice_app() {
        let decision = decide_default_audio(&playback_streams(&dump()), &["discord".to_owned()]);
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
        let decision = decide_default_audio(
            &playback_streams(&dump_without_discord_playing()),
            &["discord".to_owned()],
        );
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
            decide_default_audio(&playback_streams(&voice_only), &["discord".to_owned()]),
            DefaultAudioDecision::NoAudio(NoAudioReason::OnlyExcludedPlaying)
        );
    }

    #[test]
    fn default_shares_no_audio_when_nothing_plays() {
        assert_eq!(
            decide_default_audio(&[], &["discord".to_owned()]),
            DefaultAudioDecision::NoAudio(NoAudioReason::NothingPlaying)
        );
    }

    // Windows process trees. The snapshot logic is plain data, so these run
    // on every platform.

    fn process(id: u32, parent: u32, executable: &str, created: u64) -> Process {
        Process {
            id,
            parent,
            executable: executable.to_owned(),
            created: Some(created),
        }
    }

    fn kept_names(streams: &[PlaybackStream], excluded: &[&str]) -> Vec<String> {
        let excluded: Vec<String> = excluded.iter().map(|name| (*name).to_owned()).collect();
        match decide_default_audio(streams, &excluded) {
            DefaultAudioDecision::Share(kept) => kept.iter().map(|app| app.label.clone()).collect(),
            DefaultAudioDecision::NoAudio(_) => Vec::new(),
        }
    }

    /// Discord launched from Steam sits inside Steam's process tree, so
    /// capturing Steam's tree would carry a later Discord call along. Steam
    /// is excluded with it; unrelated apps still play.
    #[test]
    fn a_tree_containing_an_excluded_app_is_not_captured() {
        let processes = [
            process(4, 0, "explorer", 1),
            process(100, 4, "steam", 10),
            process(200, 100, "Discord", 20),
            process(300, 4, "Spotify", 30),
        ];
        let sessions = [(100, None), (300, Some("Spotify".to_owned()))];
        let streams = windows_streams(&sessions, &processes, 999);
        assert_eq!(kept_names(&streams, &["discord"]), ["Spotify"]);
    }

    /// The same excluded app also excludes its own session (a call playing
    /// in a Discord child process).
    #[test]
    fn an_excluded_apps_own_session_is_not_captured() {
        let processes = [
            process(4, 0, "explorer", 1),
            process(200, 4, "Discord", 20),
            process(201, 200, "Discord", 21),
            process(300, 4, "Spotify", 30),
        ];
        let sessions = [(201, None), (300, None)];
        let streams = windows_streams(&sessions, &processes, 999);
        assert_eq!(kept_names(&streams, &["discord"]), ["Spotify"]);
    }

    /// A browser's parent and child both holding sessions: the parent's tree
    /// capture already carries the child, so the child is not captured twice.
    #[test]
    fn a_kept_descendant_is_not_captured_twice() {
        let processes = [
            process(4, 0, "explorer", 1),
            process(10, 4, "chrome", 10),
            process(11, 10, "chrome", 11),
        ];
        let sessions = [(10, None), (11, None)];
        let streams = windows_streams(&sessions, &processes, 999);
        let DefaultAudioDecision::Share(kept) = decide_default_audio(&streams, &[]) else {
            panic!("chrome is playing");
        };
        assert_eq!(kept.iter().map(|app| app.serial).collect::<Vec<_>>(), [10]);
    }

    /// Process ids are recycled: a game whose id once belonged to Discord's
    /// long-gone launcher is not Discord's parent, and stays shareable.
    #[test]
    fn a_recycled_parent_id_is_not_ancestry() {
        let processes = [
            process(4, 0, "explorer", 1),
            // Discord's real parent (id 500) exited; a game started later
            // and was given the same id.
            process(200, 500, "Discord", 20),
            process(500, 4, "game", 90),
        ];
        let sessions = [(500, None)];
        let streams = windows_streams(&sessions, &processes, 999);
        assert_eq!(kept_names(&streams, &["discord"]), ["game"]);
    }

    #[test]
    fn this_process_and_exited_processes_are_skipped() {
        let processes = [process(4, 0, "explorer", 1), process(42, 4, "clarity", 5)];
        let sessions = [(0, None), (42, None), (77, None)];
        assert!(windows_streams(&sessions, &processes, 42).is_empty());
    }

    #[test]
    fn a_session_name_labels_the_stream_and_the_executable_still_matches() {
        let processes = [process(4, 0, "explorer", 1), process(300, 4, "Spotify", 30)];
        let sessions = [(300, Some("Spotify Premium".to_owned()))];
        let streams = windows_streams(&sessions, &processes, 999);
        let application = streams[0].application();
        assert_eq!(application.display_name(), "Spotify Premium (Spotify)");
        assert_eq!(application.serial, 300);
        assert!(resolve_in(&streams, "spotify").is_ok());
    }
}
