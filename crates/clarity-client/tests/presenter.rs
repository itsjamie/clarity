//! Presenter and viewer sessions, end to end through a real server: a synthetic
//! broadcast reaches a viewer that negotiates all the way to `Live`. This is the
//! session glue the GUI's presenter drives (with a real capture in place of the
//! synthetic source).

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use clarity_client::invite::{Invitation, parse_invitation};
use clarity_client::presenter::{
    PresenterCommand, PresenterEnd, PresenterError, PresenterSession, PresenterSessionConfig,
    PresenterUpdate,
};
use clarity_client::rooms::{RoomOptions, ServerEndpoints, create_room, server_endpoints};
use clarity_client::signaling::{SessionIdentity, SignalingState};
use clarity_client::viewer::{
    EndReason, ViewerCommand, ViewerError, ViewerPhase, ViewerSession, ViewerSessionConfig,
    ViewerUpdate,
};
use clarity_client::{AudioCapture, SourceConfig, SyntheticSource, VideoCodecPreference};
use clarity_core::{RoomActorConfig, TurnConfig};
use clarity_protocol::{CreateRoomResponse, RoomAccessPolicy, SharingState};
use clarity_server::{AppConfig, AppState, build_router, config::Environment};
use secrecy::SecretString;
use time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use url::Url;

struct TestServer {
    http_base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn_server_with(room_actor: RoomActorConfig) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let mut config = test_config(address);
    config.room_actor = room_actor;
    let app =
        build_router(AppState::new(config)).into_make_service_with_connect_info::<SocketAddr>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer {
        http_base: format!("http://{address}"),
        task,
    }
}

async fn spawn_server() -> TestServer {
    spawn_server_with(RoomActorConfig::default()).await
}

fn test_config(address: SocketAddr) -> AppConfig {
    let base = format!("http://{address}");
    AppConfig {
        environment: Environment::Development,
        bind_address: address,
        public_base_url: Url::parse(&base).expect("url"),
        allowed_origins: HashSet::from([base]),
        log_level: "off".into(),
        default_room_ttl: Duration::hours(2),
        maximum_room_ttl: Duration::hours(8),
        room_actor: RoomActorConfig::default(),
        room_token_hmac_key: SecretString::from(
            "test-room-token-hmac-key-at-least-32-characters".to_owned(),
        ),
        resume_token_hmac_key: SecretString::from(
            "test-resume-token-hmac-key-at-least-32-characters".to_owned(),
        ),
        websocket_auth_timeout: std::time::Duration::from_secs(2),
        websocket_heartbeat_interval: std::time::Duration::from_secs(30),
        websocket_heartbeat_timeout: std::time::Duration::from_secs(10),
        websocket_max_message_bytes: 262_144,
        sdp_max_bytes: 65_536,
        ice_candidate_max_bytes: 4_096,
        room_creation_rate_limit: 100,
        websocket_connection_rate_limit: 100,
        auth_rate_limit: 100,
        signal_rate_limit: 10_000,
        turn: TurnConfig {
            urls: vec!["stun:turn.example.test:3478".into()],
            shared_secret: SecretString::from(
                "test-turn-shared-secret-at-least-32-characters".to_owned(),
            ),
            credential_ttl: Duration::hours(1),
        },
    }
}

/// A TCP relay whose live connections can be severed on demand while its
/// listener keeps accepting, so a client's reconnect path can be exercised
/// against an untouched server.
struct FlakyProxy {
    address: SocketAddr,
    connections: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FlakyProxy {
    fn drop(&mut self) {
        self.task.abort();
        self.cut();
    }
}

impl FlakyProxy {
    async fn spawn(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("proxy bind");
        let address = listener.local_addr().expect("proxy addr");
        let connections: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> = Arc::default();
        let register = connections.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let relay = tokio::spawn(async move {
                    if let Ok(mut outbound) = TcpStream::connect(upstream).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
                register.lock().expect("proxy lock").push(relay);
            }
        });
        Self {
            address,
            connections,
            task,
        }
    }

    /// Severs every live connection; the listener stays up for reconnects.
    fn cut(&self) {
        for relay in self.connections.lock().expect("proxy lock").drain(..) {
            relay.abort();
        }
    }
}

fn synthetic_source() -> SourceConfig {
    SourceConfig::Synthetic(SyntheticSource {
        width: 640,
        height: 360,
        frame_rate: 15,
    })
}

async fn create_test_room(base: &Url, options: RoomOptions) -> CreateRoomResponse {
    create_room(base, options).await.expect("create room")
}

fn public_room() -> RoomOptions {
    RoomOptions {
        maximum_viewers: 4,
        expires_in_seconds: 3_600,
        access_policy: RoomAccessPolicy::Public,
        allowed_friend_codes: Vec::new(),
    }
}

type PresenterHandle = (
    mpsc::UnboundedSender<PresenterCommand>,
    mpsc::UnboundedReceiver<PresenterUpdate>,
    tokio::task::JoinHandle<Result<PresenterEnd, PresenterError>>,
);

fn start_presenter(
    room: &CreateRoomResponse,
    endpoints: &ServerEndpoints,
    source: Option<SourceConfig>,
    auto_approve: bool,
) -> PresenterHandle {
    let (updates, update_receiver) = mpsc::unbounded_channel();
    let (commands, command_receiver) = mpsc::unbounded_channel();
    let session = PresenterSession::start(
        PresenterSessionConfig {
            room_id: room.room_id.clone(),
            presenter_secret: SecretString::from(room.presenter_secret.clone()),
            signaling_url: endpoints.signaling_url.clone(),
            origin: endpoints.origin.clone(),
            source,
            audio: AudioCapture::Disabled,
            video_codec: VideoCodecPreference::Vp8,
            frame_rate: 15,
            capture_ceiling: None,
            bitrate_kbps: 1_000,
            adaptive: false,
            force_relay: false,
            preview_frames: None,
            display_name: Some("Host".to_owned()),
            auto_approve,
        },
        updates,
        command_receiver,
    );
    (commands, update_receiver, tokio::spawn(session.run()))
}

type ViewerHandle = (
    mpsc::UnboundedSender<ViewerCommand>,
    mpsc::UnboundedReceiver<ViewerUpdate>,
    tokio::task::JoinHandle<Result<EndReason, ViewerError>>,
);

fn start_viewer(
    invitation: Invitation,
    name: &str,
    identity: Option<SessionIdentity>,
) -> ViewerHandle {
    let (updates, update_receiver) = mpsc::unbounded_channel();
    let (commands, command_receiver) = mpsc::unbounded_channel();
    let session = ViewerSession::start(
        ViewerSessionConfig {
            invitation,
            display_name: Some(name.to_owned()),
            identity,
            force_relay: false,
            frames: None,
            native: None,
        },
        updates,
        command_receiver,
    );
    (commands, update_receiver, tokio::spawn(session.run()))
}

/// Waits until `matches` accepts an update, or the timeout passes.
async fn wait_update<U>(
    updates: &mut mpsc::UnboundedReceiver<U>,
    seconds: u64,
    mut matches: impl FnMut(&U) -> bool,
) -> bool {
    tokio::time::timeout(std::time::Duration::from_secs(seconds), async {
        while let Some(update) = updates.recv().await {
            if matches(&update) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

/// Waits for a chat update accepted by `matches`, invoking `resend` on a tick
/// since the data channel may open a moment after media begins to flow.
async fn wait_chat<U>(
    updates: &mut mpsc::UnboundedReceiver<U>,
    mut matches: impl FnMut(&U) -> bool,
    resend: impl Fn(),
) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return false,
            _ = ticker.tick() => resend(),
            update = updates.recv() => match update {
                Some(update) if matches(&update) => return true,
                Some(_) => {}
                None => return false,
            }
        }
    }
}

fn presenter_chat_containing(needle: &'static str) -> impl FnMut(&PresenterUpdate) -> bool {
    move |update| matches!(update, PresenterUpdate::Chat { text, .. } if text.contains(needle))
}

fn viewer_chat_containing(needle: &'static str) -> impl FnMut(&ViewerUpdate) -> bool {
    move |update| matches!(update, ViewerUpdate::Chat { text, .. } if text.contains(needle))
}

#[tokio::test]
async fn presenter_synthetic_broadcast_reaches_a_viewer() {
    // Decode and negotiate without opening windows or audio devices.
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let server = spawn_server().await;
    let base = Url::parse(&server.http_base).expect("url");
    let room = create_test_room(
        &base,
        RoomOptions {
            access_policy: RoomAccessPolicy::ApprovalRequired,
            ..public_room()
        },
    )
    .await;
    let endpoints = server_endpoints(&base).expect("endpoints");

    // Presenter with a synthetic source, approving join requests as they come.
    let (presenter_commands, mut presenter_rx, presenter_task) =
        start_presenter(&room, &endpoints, Some(synthetic_source()), true);

    // Viewer from the room's shareable URL.
    let invitation = parse_invitation(&room.viewer_url).expect("parse invitation");
    let (viewer_commands, mut viewer_rx, viewer_task) = start_viewer(invitation, "Viewer", None);

    let reached_live = wait_update(&mut viewer_rx, 20, |update| {
        matches!(update, ViewerUpdate::Phase(ViewerPhase::Live))
    })
    .await;
    assert!(
        reached_live,
        "the viewer did not reach Live; if this machine lacks VP8 GStreamer plugins the media \
         path cannot negotiate"
    );

    // Chat both ways over the data channel, wrapped in the ChatMessage
    // envelope by the sessions.
    let viewer_to_host = wait_chat(
        &mut presenter_rx,
        presenter_chat_containing("from the viewer"),
        || {
            let _ = viewer_commands.send(ViewerCommand::Chat("from the viewer".to_owned()));
        },
    )
    .await;
    assert!(
        viewer_to_host,
        "the presenter never received the viewer's chat"
    );

    let host_to_viewer = wait_chat(
        &mut viewer_rx,
        viewer_chat_containing("from the host"),
        || {
            let _ = presenter_commands.send(PresenterCommand::Chat("from the host".to_owned()));
        },
    )
    .await;
    assert!(
        host_to_viewer,
        "the viewer never received the presenter's chat"
    );

    let _ = viewer_commands.send(ViewerCommand::Leave);
    let _ = presenter_commands.send(PresenterCommand::CloseRoom);
    let _ = viewer_task.await;
    let _ = presenter_task.await;
}

#[tokio::test]
async fn idle_room_shares_stops_and_reshares_without_closing() {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let server = spawn_server().await;
    let base = Url::parse(&server.http_base).expect("url");
    let room = create_test_room(&base, public_room()).await;
    let endpoints = server_endpoints(&base).expect("endpoints");

    // The room opens without any source: the broadcast idles.
    let (presenter_commands, mut presenter_rx, presenter_task) =
        start_presenter(&room, &endpoints, None, true);
    let invitation = parse_invitation(&room.viewer_url).expect("parse invitation");
    let (viewer_commands, mut viewer_rx, viewer_task) = start_viewer(invitation, "Viewer", None);

    // The viewer connects to the idle room and sees it idle.
    let reached_live = wait_update(&mut viewer_rx, 20, |update| {
        matches!(update, ViewerUpdate::Phase(ViewerPhase::Live))
    })
    .await;
    assert!(reached_live, "the viewer did not connect to the idle room");

    // Chat flows while nothing is shared.
    let idle_chat = wait_chat(
        &mut presenter_rx,
        presenter_chat_containing("hello while idle"),
        || {
            let _ = viewer_commands.send(ViewerCommand::Chat("hello while idle".to_owned()));
        },
    )
    .await;
    assert!(idle_chat, "chat did not flow in the idle room");

    // Share, and the viewer sees the room go live.
    let _ = presenter_commands.send(PresenterCommand::StartShare(synthetic_source()));
    let went_live = wait_update(&mut viewer_rx, 10, |update| {
        matches!(update, ViewerUpdate::SharingState(SharingState::Live))
    })
    .await;
    assert!(went_live, "the viewer never saw sharing go live");

    // Stop sharing: the room stays open, and chat still flows.
    let _ = presenter_commands.send(PresenterCommand::StopShare);
    let back_to_idle = wait_update(&mut viewer_rx, 10, |update| {
        matches!(update, ViewerUpdate::SharingState(SharingState::Idle))
    })
    .await;
    assert!(back_to_idle, "the viewer never saw sharing stop");
    let chat_after_stop = wait_chat(&mut viewer_rx, viewer_chat_containing("still here"), || {
        let _ = presenter_commands.send(PresenterCommand::Chat("still here".to_owned()));
    })
    .await;
    assert!(
        chat_after_stop,
        "chat stopped flowing after the share ended"
    );

    // Share again, then close the room for everyone.
    let _ = presenter_commands.send(PresenterCommand::StartShare(synthetic_source()));
    let live_again = wait_update(&mut viewer_rx, 10, |update| {
        matches!(update, ViewerUpdate::SharingState(SharingState::Live))
    })
    .await;
    assert!(live_again, "the viewer never saw the re-share go live");

    let _ = presenter_commands.send(PresenterCommand::CloseRoom);
    let presenter_end = presenter_task
        .await
        .expect("presenter task")
        .expect("presenter end");
    assert_eq!(presenter_end, PresenterEnd::RoomClosed);
    let viewer_end = viewer_task.await.expect("viewer task").expect("viewer end");
    assert_eq!(viewer_end, EndReason::RoomClosed);
}

#[tokio::test]
async fn join_requests_wait_for_explicit_approval() {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let server = spawn_server().await;
    let base = Url::parse(&server.http_base).expect("url");
    let room = create_test_room(
        &base,
        RoomOptions {
            access_policy: RoomAccessPolicy::ApprovalRequired,
            ..public_room()
        },
    )
    .await;
    let endpoints = server_endpoints(&base).expect("endpoints");

    let (presenter_commands, mut presenter_rx, presenter_task) =
        start_presenter(&room, &endpoints, None, false);

    let invitation = parse_invitation(&room.viewer_url).expect("parse invitation");
    let (_viewer_commands, mut viewer_rx, viewer_task) = start_viewer(invitation, "Asker", None);

    // The request surfaces but is not answered.
    let mut requested_peer = None;
    let requested = wait_update(&mut presenter_rx, 10, |update| {
        if let PresenterUpdate::JoinRequested { peer_id, .. } = update {
            requested_peer = Some(peer_id.clone());
            true
        } else {
            false
        }
    })
    .await;
    assert!(requested, "the join request never surfaced");
    let requested_peer = requested_peer.expect("requested peer id");

    // Nothing is approved without a command.
    let auto_approved = wait_update(&mut presenter_rx, 1, |update| {
        matches!(update, PresenterUpdate::ViewerJoined { .. })
    })
    .await;
    assert!(!auto_approved, "the viewer was approved without a command");

    let _ = presenter_commands.send(PresenterCommand::ApproveViewer(requested_peer));
    let negotiating = wait_update(&mut viewer_rx, 10, |update| {
        matches!(
            update,
            ViewerUpdate::Phase(ViewerPhase::Negotiating | ViewerPhase::Live)
        )
    })
    .await;
    assert!(negotiating, "the approved viewer never started negotiating");

    // A second request is rejected explicitly.
    let invitation = parse_invitation(&room.viewer_url).expect("parse invitation");
    let (_second_commands, _second_rx, second_task) = start_viewer(invitation, "Denied", None);
    let mut second_peer = None;
    let second_requested = wait_update(&mut presenter_rx, 10, |update| {
        if let PresenterUpdate::JoinRequested { peer_id, .. } = update {
            second_peer = Some(peer_id.clone());
            true
        } else {
            false
        }
    })
    .await;
    assert!(second_requested, "the second join request never surfaced");
    let _ = presenter_commands.send(PresenterCommand::RejectViewer(
        second_peer.expect("second peer id"),
    ));
    let second_end = tokio::time::timeout(std::time::Duration::from_secs(10), second_task)
        .await
        .expect("second viewer finished")
        .expect("second viewer task")
        .expect("second viewer end");
    assert_eq!(second_end, EndReason::Rejected);

    let _ = presenter_commands.send(PresenterCommand::CloseRoom);
    let _ = presenter_task.await;
    let _ = viewer_task.await;
}

#[tokio::test]
async fn viewer_resume_expiry_falls_back_to_fresh_authentication() {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    // A viewer grace too short to ever resume; the presenter keeps the
    // default so its own reconnect resumes normally.
    let server = spawn_server_with(RoomActorConfig {
        viewer_resume_grace: Duration::milliseconds(1),
        ..RoomActorConfig::default()
    })
    .await;
    let base = Url::parse(&server.http_base).expect("url");
    let room = create_test_room(&base, public_room()).await;
    let endpoints = server_endpoints(&base).expect("endpoints");

    let (presenter_commands, _presenter_rx, presenter_task) =
        start_presenter(&room, &endpoints, Some(synthetic_source()), true);

    // The viewer's signaling runs through a proxy so its connection can be
    // severed without touching the server.
    let upstream = Url::parse(&server.http_base).expect("url");
    let upstream_address: SocketAddr = format!(
        "{}:{}",
        upstream.host_str().expect("host"),
        upstream.port().expect("port")
    )
    .parse()
    .expect("upstream address");
    let proxy = FlakyProxy::spawn(upstream_address).await;
    let mut invitation = parse_invitation(&room.viewer_url).expect("parse invitation");
    invitation.signaling_url = format!("ws://{}/api/v1/ws", proxy.address);

    let (viewer_commands, mut viewer_rx, viewer_task) = start_viewer(invitation, "Viewer", None);
    let reached_live = wait_update(&mut viewer_rx, 20, |update| {
        matches!(update, ViewerUpdate::Phase(ViewerPhase::Live))
    })
    .await;
    assert!(
        reached_live,
        "the viewer did not reach Live before the outage"
    );

    // Sever signaling. The resume is rejected (the grace window is over), and
    // the session must fall back to a fresh join instead of dying.
    proxy.cut();
    let reconnecting = wait_update(&mut viewer_rx, 10, |update| {
        matches!(
            update,
            ViewerUpdate::Signaling(SignalingState::Reconnecting)
        )
    })
    .await;
    assert!(
        reconnecting,
        "the viewer never noticed the severed connection"
    );

    let live_again = wait_update(&mut viewer_rx, 30, |update| {
        matches!(update, ViewerUpdate::Phase(ViewerPhase::Live))
    })
    .await;
    assert!(
        live_again,
        "the viewer did not recover to Live after its resumable session expired"
    );

    let _ = viewer_commands.send(ViewerCommand::Leave);
    let viewer_end = tokio::time::timeout(std::time::Duration::from_secs(10), viewer_task)
        .await
        .expect("viewer finished")
        .expect("viewer task")
        .expect("viewer end");
    assert_eq!(viewer_end, EndReason::Left);
    let _ = presenter_commands.send(PresenterCommand::CloseRoom);
    let _ = presenter_task.await;
}

#[tokio::test]
async fn friends_only_room_admits_a_proven_friend() {
    unsafe { std::env::set_var("CLARITY_MEDIA_HEADLESS", "1") };

    let server = spawn_server().await;
    let base = Url::parse(&server.http_base).expect("url");
    let identity = clarity_identity::Identity::create("Friend", "Test").expect("identity");
    let room = create_test_room(
        &base,
        RoomOptions {
            access_policy: RoomAccessPolicy::FriendsOnly,
            allowed_friend_codes: vec![identity.friend_code()],
            ..public_room()
        },
    )
    .await;

    // A viewer that can sign the identity challenge authenticates and waits
    // in the room like any other; no presenter is required for that.
    let signer = identity.clone();
    let session_identity = SessionIdentity {
        public_key: identity.public_key().to_vec(),
        sign: Arc::new(move |message| signer.sign(message)),
    };
    let invitation = parse_invitation(&room.viewer_url).expect("parse invitation");
    let (_viewer_commands, mut viewer_rx, viewer_task) =
        start_viewer(invitation, "Friend", Some(session_identity));
    let authenticated = wait_update(&mut viewer_rx, 10, |update| {
        matches!(update, ViewerUpdate::Phase(_))
    })
    .await;
    assert!(
        authenticated,
        "the proven friend was not admitted to the friends-only room"
    );
    viewer_task.abort();

    // Without an identity the challenge goes unanswered and authentication
    // fails within the server's auth timeout.
    let invitation = parse_invitation(&room.viewer_url).expect("parse invitation");
    let (_stranger_commands, _stranger_rx, stranger_task) =
        start_viewer(invitation, "Stranger", None);
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), stranger_task)
        .await
        .expect("stranger finished")
        .expect("stranger task");
    assert!(
        matches!(outcome, Err(ViewerError::AuthenticationFailed(_))),
        "a viewer without an identity was not turned away: {outcome:?}"
    );
}
