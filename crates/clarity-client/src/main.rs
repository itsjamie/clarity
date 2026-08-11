use anyhow::Context;
use clap::{Parser, Subcommand};
use clarity_client::invite::parse_invitation;
use clarity_client::presenter::{
    PresenterCommand, PresenterEnd, PresenterSession, PresenterSessionConfig, PresenterUpdate,
};
use clarity_client::rooms::{RoomOptions, create_room, server_endpoints};
use clarity_client::signaling::SignalingState;
use clarity_client::viewer::{
    EndReason, ViewerCommand, ViewerPhase, ViewerSession, ViewerSessionConfig, ViewerUpdate,
};
use clarity_media::{
    AudioCapture, CaptureError, CaptureRequest, CaptureStream, SourceConfig, SyntheticSource,
    VideoCodecId,
};
use clarity_protocol::{RoomAccessPolicy, SharingState};
use secrecy::SecretString;
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Parser)]
#[command(name = "clarity", version, about = "Native client for Clarity Share")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum CodecArg {
    Auto,
    Av1,
    H265,
    H264,
    Vp9,
    Vp8,
}

impl CodecArg {
    /// `Auto` offers the default ranking; a specific codec pins the offer to
    /// exactly that codec (viewers that cannot decode it get no video).
    fn ranking(self) -> Vec<VideoCodecId> {
        match self {
            Self::Auto => Vec::new(),
            Self::Av1 => vec![VideoCodecId::Av1],
            Self::H265 => vec![VideoCodecId::H265],
            Self::H264 => vec![VideoCodecId::H264],
            Self::Vp9 => vec![VideoCodecId::Vp9],
            Self::Vp8 => vec![VideoCodecId::Vp8],
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Watch a shared screen from a viewer invitation link.
    View {
        /// The complete viewer invitation URL, including everything after `#`.
        invitation: String,
        /// Display name shown to the presenter.
        #[arg(long)]
        name: Option<String>,
        /// Restrict WebRTC to TURN-relayed paths (connectivity diagnostic).
        #[arg(long)]
        force_relay: bool,
    },
    /// Create a room and share a screen or window with its viewers.
    /// Ctrl+C ends the room for everyone.
    Present {
        /// Base URL of the Clarity server.
        #[arg(long, default_value = "http://127.0.0.1:3000")]
        server: String,
        /// Maximum number of viewers.
        #[arg(long, default_value_t = 4)]
        viewers: u8,
        /// Room lifetime in hours (1-8).
        #[arg(long, default_value_t = 1)]
        hours: u32,
        /// Require explicit approval before viewers join. Requests are
        /// approved automatically unless --no-auto-approve is also given.
        #[arg(long)]
        approval: bool,
        /// Approve nothing automatically; answer join requests with
        /// `/approve <peer>` or `/deny <peer>` on stdin.
        #[arg(long)]
        no_auto_approve: bool,
        /// Restrict the room to these friend codes (repeatable). Viewers must
        /// prove their identity to join.
        #[arg(long = "friend", value_name = "CODE")]
        friends: Vec<String>,
        /// Open the room without sharing anything yet; viewers can join and
        /// chat while the room is idle.
        #[arg(long, conflicts_with = "synthetic")]
        idle: bool,
        /// Display name attached to your chat messages.
        #[arg(long)]
        name: Option<String>,
        /// Video codec preference. `auto` offers hardware AV1, H.265, and
        /// H.264, then software VP9 and VP8, and each viewer takes the best
        /// it can decode; a specific codec pins the offer to it alone.
        #[arg(long, value_enum, default_value_t = CodecArg::Auto)]
        codec: CodecArg,
        /// Per-viewer video bitrate ceiling in kilobits per second.
        #[arg(long, default_value_t = 8000)]
        bitrate_kbps: u32,
        /// Hold the bitrate ceiling instead of adapting each viewer's rate to
        /// its network feedback.
        #[arg(long)]
        fixed_bitrate: bool,
        /// Share the picture only, without the system's audio.
        #[arg(long)]
        no_audio: bool,
        /// Share one application's audio instead of the system mix. With a
        /// name, matches it against playing applications (e.g. "spotify");
        /// without one, offers a pick from whatever is playing right now.
        #[arg(long, value_name = "NAME", num_args = 0..=1, conflicts_with_all = ["no_audio", "audio_except"])]
        audio_app: Option<Option<String>>,
        /// Share the system's audio with these applications removed (e.g.
        /// "discord" to keep a voice call private). Repeatable. The shared
        /// mix follows what is playing: applications that start later join
        /// it, the excluded ones never do. Overrides the default, which
        /// already excludes a voice-chat app this way.
        #[arg(long, value_name = "NAME", conflicts_with = "no_audio")]
        audio_except: Vec<String>,
        /// Share the full system mix, including the voice-chat app the
        /// default keeps private.
        #[arg(long, conflicts_with_all = ["no_audio", "audio_app", "audio_except"])]
        all_audio: bool,
        /// Leave the mouse cursor out of the shared picture.
        #[arg(long)]
        hide_cursor: bool,
        /// Remember the chosen screen or window and reuse it on later runs
        /// without asking.
        #[arg(long)]
        remember: bool,
        /// Show the picker even when a remembered choice exists; the new
        /// choice replaces it if --remember is also set.
        #[arg(long)]
        pick: bool,
        /// Broadcast a synthetic test pattern instead of capturing a screen.
        #[arg(long)]
        synthetic: bool,
        /// Synthetic pattern dimensions and rate (with --synthetic).
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        #[arg(long, default_value_t = 30)]
        fps: u32,
        /// Restrict WebRTC to TURN-relayed paths (connectivity diagnostic).
        #[arg(long)]
        force_relay: bool,
    },
    /// List the applications currently playing audio, usable with
    /// `present --audio-app`.
    AudioApps,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::View {
            invitation,
            name,
            force_relay,
        } => view(invitation, name, force_relay).await,
        Command::Present {
            server,
            viewers,
            hours,
            approval,
            no_auto_approve,
            friends,
            idle,
            name,
            codec,
            bitrate_kbps,
            fixed_bitrate,
            no_audio,
            audio_app,
            audio_except,
            all_audio,
            hide_cursor,
            remember,
            pick,
            synthetic,
            width,
            height,
            fps,
            force_relay,
        } => {
            let (audio, audio_exclude) = if no_audio {
                (AudioCapture::Disabled, None)
            } else if all_audio || synthetic {
                (AudioCapture::SystemMix, None)
            } else if let Some(request) = audio_app {
                let application = match request {
                    Some(name) => clarity_client::audio_apps::resolve_application(&name)?,
                    None => choose_audio_app()?,
                };
                info!(app = %application.display_name(), "sharing this application's audio");
                (
                    AudioCapture::Streams {
                        targets: vec![application.serial.to_string()],
                    },
                    None,
                )
            } else if !audio_except.is_empty() {
                // Strict at start (a typo must not share the audio it meant
                // to remove), then watched: the exclusion holds for the whole
                // share, and applications that start playing later join the
                // mix.
                (audio_mix_except(&audio_except)?, Some(audio_except.clone()))
            } else {
                // Keep a voice-chat app out of the shared mix by default.
                default_audio()
            };
            let source = if idle {
                None
            } else if synthetic {
                Some(SourceConfig::Synthetic(SyntheticSource {
                    width,
                    height,
                    frame_rate: fps,
                }))
            } else {
                Some(SourceConfig::Screen(
                    open_capture(!hide_cursor, remember, pick).await?,
                ))
            };
            let access_policy = if !friends.is_empty() {
                RoomAccessPolicy::FriendsOnly
            } else if approval {
                RoomAccessPolicy::ApprovalRequired
            } else {
                RoomAccessPolicy::Public
            };
            present(
                server,
                RoomOptions {
                    maximum_viewers: viewers,
                    expires_in_seconds: hours.saturating_mul(3600),
                    access_policy,
                    allowed_friend_codes: friends,
                },
                MediaOptions {
                    source,
                    audio,
                    audio_exclude,
                    video_codecs: codec.ranking(),
                    frame_rate: fps,
                    bitrate_kbps,
                    adaptive: !fixed_bitrate,
                    force_relay,
                },
                PresenterOptions {
                    display_name: name,
                    auto_approve: !no_auto_approve,
                },
            )
            .await
        }
        Command::AudioApps => {
            let applications = clarity_client::audio_apps::playing_applications()?;
            if applications.is_empty() {
                println!("Nothing is playing audio right now.");
            } else {
                println!("Applications playing audio:");
                for application in &applications {
                    println!("  {}", application.display_name());
                }
                println!("\nShare one with: clarity present --audio-app <name>");
            }
            Ok(())
        }
    }
}

/// Voice-chat applications kept out of the shared mix unless the presenter
/// asks for them, so an ongoing call is not broadcast by accident.
/// The audio for a share with no explicit audio flag: the currently-playing
/// applications' streams with the default voice-chat apps removed, kept
/// current by the session's watchdog — applications that start playing later
/// join the mix, and the excluded voice app stays out however late its call
/// starts. Streams are captured individually, never as the device monitor;
/// falling back to the monitor here is exactly how an idle-at-start Discord
/// used to become audible the moment a call began. `--all-audio` is the
/// explicit way to share the full monitor mix.
fn default_audio() -> (AudioCapture, Option<Vec<String>>) {
    use clarity_client::audio_apps::{
        DefaultAudioDecision, NoAudioReason, default_audio_exclusion, default_excluded,
    };
    let excluded = default_excluded();
    match default_audio_exclusion(&excluded) {
        DefaultAudioDecision::Share(kept) => {
            info!(
                sharing = %kept.iter().map(|app| app.display_name()).collect::<Vec<_>>().join(", "),
                excluded = %excluded.join(", "),
                "sharing the audio playing now, voice chat excluded (--all-audio shares everything)"
            );
            (
                AudioCapture::Streams {
                    targets: kept.iter().map(|app| app.serial.to_string()).collect(),
                },
                Some(excluded),
            )
        }
        DefaultAudioDecision::NoAudio(NoAudioReason::OnlyExcludedPlaying) => {
            info!("only voice-chat audio is playing; sharing silence until something else plays");
            (AudioCapture::Streams { targets: vec![] }, Some(excluded))
        }
        DefaultAudioDecision::NoAudio(NoAudioReason::NothingPlaying) => {
            info!(
                "nothing is playing audio yet; whatever plays next is shared \
                 (voice chat stays excluded; --all-audio shares everything)"
            );
            (AudioCapture::Streams { targets: vec![] }, Some(excluded))
        }
        DefaultAudioDecision::NoAudio(NoAudioReason::GraphUnreadable(reason)) => {
            warn!(
                %reason,
                "the audio graph is unreadable, so voice chat cannot be excluded; \
                 sharing without audio (--all-audio shares everything unconditionally)"
            );
            (AudioCapture::Disabled, None)
        }
    }
}

/// Resolves an explicit `--audio-except` set: strict, so a mistyped name is an
/// error rather than silently sharing the audio it was meant to remove.
fn audio_mix_except(excluded: &[String]) -> anyhow::Result<AudioCapture> {
    let kept = clarity_client::audio_apps::applications_except(excluded)?;
    if kept.is_empty() {
        warn!("only the excluded applications are playing; sharing without audio");
        return Ok(AudioCapture::Disabled);
    }
    info!(
        sharing = %kept.iter().map(|app| app.display_name()).collect::<Vec<_>>().join(", "),
        "sharing these applications' audio"
    );
    Ok(AudioCapture::Streams {
        targets: kept.iter().map(|app| app.serial.to_string()).collect(),
    })
}

/// Numbered pick from the applications currently playing audio; a single
/// playing application is chosen without asking.
fn choose_audio_app() -> anyhow::Result<clarity_client::audio_apps::AudioApplication> {
    use std::io::Write;
    let applications = clarity_client::audio_apps::playing_applications()?;
    match applications.len() {
        0 => anyhow::bail!(
            "nothing is playing audio right now; start playback in the application first"
        ),
        1 => {
            return Ok(applications
                .into_iter()
                .next()
                .expect("one application is present"));
        }
        _ => {}
    }
    println!("\n  Applications playing audio:");
    for (index, application) in applications.iter().enumerate() {
        println!("    {}. {}", index + 1, application.display_name());
    }
    print!(
        "  Share which application's audio? [1-{}]: ",
        applications.len()
    );
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|_| anyhow::anyhow!("no selection was made"))?;
    let choice: usize = line
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("expected a number between 1 and {}", applications.len()))?;
    applications
        .into_iter()
        .nth(choice.saturating_sub(1))
        .ok_or_else(|| anyhow::anyhow!("expected a number between 1 and the listed count"))
}

/// Opens the system picker. Remembering is opt-in: without `remember`, no
/// grant is retained anywhere and the picker appears every run; with it, the
/// choice is reused on later runs unless `pick` forces the dialog again.
async fn open_capture(
    show_cursor: bool,
    remember: bool,
    pick: bool,
) -> anyhow::Result<CaptureStream> {
    let token_path = restore_token_path();
    let restore_token = if remember && !pick {
        token_path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|token| token.trim().to_owned())
            .filter(|token| !token.is_empty())
    } else {
        None
    };
    if restore_token.is_none() {
        info!("choose a screen or window to share");
    }
    let stream = CaptureStream::open(CaptureRequest {
        show_cursor,
        restore_token,
        remember,
    })
    .await
    .map_err(|error| match error {
        CaptureError::Cancelled => anyhow::anyhow!("no source was selected"),
        other => anyhow::Error::new(other),
    })?;
    if remember && let (Some(path), Some(token)) = (token_path, stream.restore_token()) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, token);
        info!("capture choice remembered; use --pick to choose differently");
    }
    Ok(stream)
}

fn restore_token_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/state"))
        })?;
    Some(base.join("clarity/capture-restore-token"))
}

/// The captured media and encoding choices for a `present` session.
struct MediaOptions {
    source: Option<SourceConfig>,
    audio: AudioCapture,
    audio_exclude: Option<Vec<String>>,
    video_codecs: Vec<VideoCodecId>,
    frame_rate: u32,
    bitrate_kbps: u32,
    adaptive: bool,
    force_relay: bool,
}

/// Session behaviour choices for a `present` session.
struct PresenterOptions {
    display_name: Option<String>,
    auto_approve: bool,
}

/// Reads stdin lines off the async runtime and forwards them; ends quietly
/// when stdin closes.
fn spawn_stdin_lines() -> mpsc::UnboundedReceiver<String> {
    let (lines, receiver) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let mut buffer = String::new();
        while std::io::stdin()
            .read_line(&mut buffer)
            .is_ok_and(|read| read > 0)
        {
            let line = buffer.trim().to_owned();
            buffer.clear();
            if !line.is_empty() && lines.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

/// Maps one stdin line to a presenter command: `/approve <peer>` and
/// `/deny <peer>` answer join requests, anything else is chat.
fn presenter_command_for(line: &str) -> PresenterCommand {
    if let Some(peer) = line.strip_prefix("/approve ") {
        PresenterCommand::ApproveViewer(peer.trim().to_owned())
    } else if let Some(peer) = line.strip_prefix("/deny ") {
        PresenterCommand::RejectViewer(peer.trim().to_owned())
    } else {
        PresenterCommand::Chat(line.to_owned())
    }
}

async fn present(
    server: String,
    options: RoomOptions,
    media: MediaOptions,
    presenter: PresenterOptions,
) -> anyhow::Result<()> {
    let server = url::Url::parse(&server).context("could not parse the server URL")?;
    let endpoints = server_endpoints(&server)?;
    let room = create_room(&server, options).await?;
    info!(room = %room.room_id, expires = %room.expires_at, "room created");
    println!("\n  Invite viewers:\n  {}\n", room.viewer_url);
    if !presenter.auto_approve {
        println!("  Approve join requests with: /approve <peer>  (or /deny <peer>)\n");
    }

    let (updates, mut update_receiver) = mpsc::unbounded_channel();
    let (commands, command_receiver) = mpsc::unbounded_channel();
    let session = PresenterSession::start(
        PresenterSessionConfig {
            room_id: room.room_id,
            presenter_secret: SecretString::from(room.presenter_secret),
            signaling_url: endpoints.signaling_url,
            origin: endpoints.origin,
            source: media.source,
            audio: media.audio,
            audio_exclude: media.audio_exclude,
            video_codecs: media.video_codecs,
            frame_rate: media.frame_rate,
            capture_ceiling: None,
            bitrate_kbps: media.bitrate_kbps,
            adaptive: media.adaptive,
            force_relay: media.force_relay,
            preview_frames: None,
            display_name: presenter.display_name,
            auto_approve: presenter.auto_approve,
        },
        updates,
        command_receiver,
    );

    let interrupt_commands = commands.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("ending the room");
            let _ = interrupt_commands.send(PresenterCommand::CloseRoom);
        }
    });
    let stdin_commands = commands.clone();
    tokio::spawn(async move {
        let mut lines = spawn_stdin_lines();
        while let Some(line) = lines.recv().await {
            if stdin_commands.send(presenter_command_for(&line)).is_err() {
                break;
            }
        }
    });
    let printer = tokio::spawn(async move {
        while let Some(update) = update_receiver.recv().await {
            report_presenter(update);
        }
    });

    let outcome = session.run().await;
    printer.abort();
    match outcome? {
        PresenterEnd::RoomClosed => info!("the room was closed"),
        PresenterEnd::RoomExpired => info!("the room expired"),
        PresenterEnd::Left => info!("left the room; it stays open until it expires"),
    }
    Ok(())
}

fn report_presenter(update: PresenterUpdate) {
    match update {
        PresenterUpdate::Signaling(state) => match state {
            SignalingState::Connecting => info!("connecting to the server"),
            SignalingState::Authenticating => info!("authenticating as presenter"),
            SignalingState::Connected => info!("connected to the room"),
            SignalingState::Reconnecting => warn!("connection lost; reconnecting"),
            SignalingState::Closed | SignalingState::Failed => {}
        },
        PresenterUpdate::SharingState(state) => match state {
            SharingState::Idle => info!("not sharing; the room stays open"),
            SharingState::Live => info!("sharing is live"),
            SharingState::Paused => info!("sharing is paused"),
        },
        PresenterUpdate::RoomExpiry { expires_in_seconds } => {
            tracing::debug!(expires_in_seconds, "room expiry");
        }
        PresenterUpdate::JoinRequested {
            peer_id,
            display_name,
            friend_code,
        } => {
            info!(
                viewer = %peer_id,
                friend = %friend_code.as_deref().unwrap_or("-"),
                "{} asked to join",
                display_name.as_deref().unwrap_or("a viewer")
            );
        }
        PresenterUpdate::ViewerJoined {
            peer_id,
            display_name,
        } => {
            info!(
                viewer = %peer_id,
                "{} joined",
                display_name.as_deref().unwrap_or("a viewer")
            );
        }
        PresenterUpdate::ViewerLeft { peer_id } => info!(viewer = %peer_id, "viewer left"),
        PresenterUpdate::Chat {
            peer_id,
            sender,
            text,
        } => info!(viewer = %peer_id, "chat: {sender}: {text}"),
        PresenterUpdate::ShareFailed { message } => warn!("sharing failed: {message}"),
        PresenterUpdate::ViewerConnection { peer_id, state } => {
            tracing::debug!(viewer = %peer_id, ?state, "viewer connection state");
        }
        PresenterUpdate::ViewerStats { peer_id, stats } => {
            info!(
                viewer = %peer_id,
                "sending {} kbps (target {}), {} lost, rtt {}",
                stats
                    .bitrate_kbps
                    .map_or_else(|| "?".to_owned(), |v| v.to_string()),
                stats.target_kbps,
                stats
                    .packets_lost
                    .map_or_else(|| "?".to_owned(), |v| v.to_string()),
                stats
                    .round_trip_ms
                    .map_or_else(|| "?".to_owned(), |v| format!("{v:.0} ms")),
            );
        }
    }
}

async fn view(
    invitation: String,
    display_name: Option<String>,
    force_relay: bool,
) -> anyhow::Result<()> {
    let invitation = parse_invitation(&invitation).context("could not use the invitation link")?;
    info!(room = %invitation.room_id, "joining room");

    let (updates, mut update_receiver) = mpsc::unbounded_channel();
    let (commands, command_receiver) = mpsc::unbounded_channel();
    let session = ViewerSession::start(
        ViewerSessionConfig {
            invitation,
            display_name,
            identity: None,
            force_relay,
            // The CLI viewer opens a native window rather than delivering frames.
            frames: None,
            native: None,
        },
        updates,
        command_receiver,
    );

    let interrupt_commands = commands.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("leaving the room");
            let _ = interrupt_commands.send(ViewerCommand::Leave);
        }
    });
    let chat_commands = commands.clone();
    tokio::spawn(async move {
        let mut lines = spawn_stdin_lines();
        while let Some(line) = lines.recv().await {
            if chat_commands.send(ViewerCommand::Chat(line)).is_err() {
                break;
            }
        }
    });
    let printer = tokio::spawn(async move {
        while let Some(update) = update_receiver.recv().await {
            report(update);
        }
    });

    let outcome = session.run().await;
    printer.abort();
    match outcome? {
        EndReason::RoomClosed => info!("the presenter ended the room"),
        EndReason::RoomExpired => info!("the room expired"),
        EndReason::Rejected => info!("the presenter declined this join request"),
        EndReason::Kicked => info!("the presenter removed you from the room"),
        EndReason::Left => info!("left the room"),
    }
    Ok(())
}

fn report(update: ViewerUpdate) {
    match update {
        ViewerUpdate::Signaling(state) => match state {
            SignalingState::Connecting => info!("connecting to the room"),
            SignalingState::Authenticating => info!("presenting the invitation"),
            SignalingState::Connected => info!("connected to the room"),
            SignalingState::Reconnecting => warn!("connection lost; reconnecting"),
            SignalingState::Closed | SignalingState::Failed => {}
        },
        ViewerUpdate::Phase(phase) => match phase {
            ViewerPhase::Connecting => {}
            ViewerPhase::AwaitingApproval => info!("waiting for the presenter to approve you"),
            ViewerPhase::Negotiating => info!("setting up the stream"),
            ViewerPhase::Live => info!("live — the stream window is open"),
        },
        ViewerUpdate::Chat { sender, text } => info!("chat: {sender}: {text}"),
        // The CLI never requests the native overlay (`native: None` above).
        ViewerUpdate::NativeSurface(_) => {}
        ViewerUpdate::SharingState(state) => match state {
            SharingState::Idle => info!("the presenter is not sharing right now"),
            SharingState::Live => {}
            SharingState::Paused => info!("the presenter paused sharing"),
        },
        ViewerUpdate::RoomExpiry { expires_in_seconds } => {
            tracing::debug!(expires_in_seconds, "room expiry");
        }
        ViewerUpdate::PresenterConnected(connected) => {
            if connected {
                info!("the presenter is connected");
            } else {
                warn!("the presenter disconnected; the room is waiting for them");
            }
        }
        ViewerUpdate::Connection(state) => tracing::debug!(?state, "peer connection state"),
        ViewerUpdate::Ice(state) => tracing::debug!(?state, "ice state"),
        ViewerUpdate::Stats(stats) => {
            let resolution = match (stats.width, stats.height) {
                (Some(width), Some(height)) => format!("{width}x{height}"),
                _ => "?".to_owned(),
            };
            info!(
                "{resolution} {codec} {bitrate} kbps, {lost} lost, rtt {rtt}",
                codec = stats.codec.as_deref().unwrap_or("?"),
                bitrate = stats
                    .bitrate_kbps
                    .map_or_else(|| "?".to_owned(), |v| v.to_string()),
                lost = stats
                    .packets_lost
                    .map_or_else(|| "?".to_owned(), |v| v.to_string()),
                rtt = stats
                    .round_trip_ms
                    .map_or_else(|| "?".to_owned(), |v| format!("{v:.0} ms")),
            );
        }
    }
}
