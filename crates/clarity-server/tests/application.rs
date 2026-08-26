use std::{collections::HashSet, net::SocketAddr};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use clarity_core::{RoomActorConfig, TurnConfig};
use clarity_protocol::{
    ClientMessage, CreateRoomRequest, CreateRoomResponse, ErrorCode, PROTOCOL_VERSION,
    RoomAccessPolicy, ServerMessage, SharingState,
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use clarity_server::{AppConfig, AppState, build_router, config::Environment};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, StatusCode};
use secrecy::SecretString;
use time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};
use url::Url;

type ClientWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct TestServer {
    base_url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn_server() -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let address = listener.local_addr().expect("local address");
    let state = AppState::new(test_config(address));
    let app = build_router(state).into_make_service_with_connect_info::<SocketAddr>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer {
        base_url: format!("http://{address}"),
        task,
    }
}

fn test_config(address: SocketAddr) -> AppConfig {
    let base_url = format!("http://{address}");
    AppConfig {
        environment: Environment::Development,
        trusted_proxy_hops: 0,
        bind_address: address,
        public_base_url: Url::parse(&base_url).expect("url"),
        allowed_origins: HashSet::from([base_url]),
        log_level: "off".into(),
        default_room_ttl: Duration::hours(2),
        maximum_room_ttl: Duration::hours(8),
        room_actor: RoomActorConfig {
            presenter_resume_grace: Duration::milliseconds(500),
            viewer_resume_grace: Duration::milliseconds(500),
            ..RoomActorConfig::default()
        },
        room_token_hmac_key: SecretString::from(
            "test-room-token-hmac-key-at-least-32-characters".to_owned(),
        ),
        resume_token_hmac_key: SecretString::from(
            "test-resume-token-hmac-key-at-least-32-characters".to_owned(),
        ),
        websocket_auth_timeout: std::time::Duration::from_millis(300),
        websocket_heartbeat_interval: std::time::Duration::from_secs(30),
        websocket_heartbeat_timeout: std::time::Duration::from_secs(10),
        websocket_max_message_bytes: 262_144,
        sdp_max_bytes: 2_048,
        ice_candidate_max_bytes: 1_024,
        room_creation_rate_limit: 100,
        websocket_connection_rate_limit: 100,
        auth_rate_limit: 100,
        signal_rate_limit: 1_000,
        turn: TurnConfig {
            urls: vec![
                "stun:turn.example.test:3478".into(),
                "turn:turn.example.test:3478?transport=udp".into(),
            ],
            shared_secret: SecretString::from(
                "test-turn-shared-secret-at-least-32-characters".to_owned(),
            ),
            credential_ttl: Duration::hours(1),
        },
    }
}

async fn create_room(server: &TestServer, maximum_viewers: u8) -> CreateRoomResponse {
    create_room_with_policy(
        server,
        maximum_viewers,
        Some(RoomAccessPolicy::ApprovalRequired),
    )
    .await
}

async fn create_public_room(server: &TestServer, maximum_viewers: u8) -> CreateRoomResponse {
    create_room_with_policy(server, maximum_viewers, None).await
}

async fn create_room_with_policy(
    server: &TestServer,
    maximum_viewers: u8,
    access_policy: Option<RoomAccessPolicy>,
) -> CreateRoomResponse {
    let response = Client::new()
        .post(format!("{}/api/v1/rooms", server.base_url))
        .header("origin", &server.base_url)
        .json(&CreateRoomRequest {
            maximum_viewers: Some(maximum_viewers),
            expires_in_seconds: Some(3_600),
            access_policy,
            allowed_friend_codes: None,
        })
        .send()
        .await
        .expect("create room request");
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()["cache-control"], "no-store");
    response.json().await.expect("room response")
}

async fn create_friends_room(server: &TestServer, allowed: Vec<String>) -> CreateRoomResponse {
    let response = Client::new()
        .post(format!("{}/api/v1/rooms", server.base_url))
        .header("origin", &server.base_url)
        .json(&CreateRoomRequest {
            maximum_viewers: Some(4),
            expires_in_seconds: Some(3_600),
            access_policy: Some(RoomAccessPolicy::FriendsOnly),
            allowed_friend_codes: Some(allowed),
        })
        .send()
        .await
        .expect("create friends room request");
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json().await.expect("room response")
}

/// A throwaway signing identity for friends-only authentication.
struct Identity {
    key_pair: Ed25519KeyPair,
}

impl Identity {
    fn new() -> Self {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("keygen");
        Self {
            key_pair: Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("from pkcs8"),
        }
    }

    fn public_key_b64(&self) -> String {
        BASE64.encode(self.key_pair.public_key().as_ref())
    }

    fn code(&self) -> String {
        clarity_protocol::code::encode(self.key_pair.public_key().as_ref())
    }

    fn sign_b64(&self, message: &str) -> String {
        BASE64.encode(self.key_pair.sign(message.as_bytes()).as_ref())
    }
}

/// Runs the viewer authentication for a friends-only room through the
/// identity challenge with `identity`, returning the final auth reply.
async fn join_friends_room(
    server: &TestServer,
    room: &CreateRoomResponse,
    identity: &Identity,
    name: &str,
) -> (ClientWebSocket, ServerMessage) {
    let invitation = Url::parse(&room.viewer_url).expect("viewer URL");
    let mut socket = connect_websocket(server).await;
    send_client(
        &mut socket,
        ClientMessage::AuthViewer {
            protocol_version: PROTOCOL_VERSION,
            request_id: format!("friends-auth-{name}"),
            room_id: room.room_id.clone(),
            viewer_secret: invitation.fragment().expect("fragment").into(),
            display_name: Some(name.into()),
        },
    )
    .await;
    let ServerMessage::AuthIdentityChallenge { nonce, .. } = next_server(&mut socket).await else {
        panic!("expected an identity challenge for a friends-only room");
    };
    // The signature is bound to the room-auth context and the server's host,
    // exactly as the real clients sign.
    let payload = clarity_protocol::identity_challenge_payload(
        clarity_protocol::IDENTITY_CONTEXT_ROOM_AUTH,
        server.base_url.trim_start_matches("http://"),
        &nonce,
    );
    send_client(
        &mut socket,
        ClientMessage::AuthIdentity {
            protocol_version: PROTOCOL_VERSION,
            request_id: format!("friends-identity-{name}"),
            public_key: identity.public_key_b64(),
            signature: identity.sign_b64(&payload),
        },
    )
    .await;
    let reply = next_server(&mut socket).await;
    (socket, reply)
}

async fn connect_websocket(server: &TestServer) -> ClientWebSocket {
    let websocket_url = server.base_url.replacen("http://", "ws://", 1) + "/api/v1/ws";
    let mut request = websocket_url
        .into_client_request()
        .expect("websocket request");
    request.headers_mut().insert(
        "origin",
        HeaderValue::from_str(&server.base_url).expect("origin"),
    );
    connect_async(request).await.expect("websocket connect").0
}

async fn send_client(socket: &mut ClientWebSocket, message: ClientMessage) {
    socket
        .send(Message::Text(
            serde_json::to_string(&message).expect("serialize").into(),
        ))
        .await
        .expect("send websocket message");
}

async fn next_server(socket: &mut ClientWebSocket) -> ServerMessage {
    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
            .await
            .expect("server message timeout")
            .expect("websocket open")
            .expect("valid websocket frame");
        if let Message::Text(text) = message {
            let parsed: ServerMessage =
                serde_json::from_str(&text).expect("server protocol message");
            if let ServerMessage::HeartbeatPing { nonce, .. } = parsed {
                send_client(
                    socket,
                    ClientMessage::HeartbeatPong {
                        protocol_version: PROTOCOL_VERSION,
                        nonce,
                    },
                )
                .await;
                continue;
            }
            return parsed;
        }
    }
}

async fn wait_for<F>(socket: &mut ClientWebSocket, predicate: F) -> ServerMessage
where
    F: Fn(&ServerMessage) -> bool,
{
    loop {
        let message = next_server(socket).await;
        if predicate(&message) {
            return message;
        }
    }
}

async fn authenticate_presenter(
    server: &TestServer,
    room: &CreateRoomResponse,
) -> (ClientWebSocket, String, String) {
    let mut socket = connect_websocket(server).await;
    send_client(
        &mut socket,
        ClientMessage::AuthPresenter {
            protocol_version: PROTOCOL_VERSION,
            request_id: "presenter-auth".into(),
            room_id: room.room_id.clone(),
            presenter_secret: room.presenter_secret.clone(),
        },
    )
    .await;
    let ServerMessage::AuthSucceeded {
        peer_id,
        resume_token,
        ..
    } = next_server(&mut socket).await
    else {
        panic!("presenter auth should succeed");
    };
    (socket, peer_id, resume_token)
}

async fn authenticate_viewer(
    server: &TestServer,
    room: &CreateRoomResponse,
    name: &str,
) -> (ClientWebSocket, String) {
    let invitation = Url::parse(&room.viewer_url).expect("viewer URL");
    let secret = invitation.fragment().expect("fragment");
    let mut socket = connect_websocket(server).await;
    send_client(
        &mut socket,
        ClientMessage::AuthViewer {
            protocol_version: PROTOCOL_VERSION,
            request_id: format!("viewer-auth-{name}"),
            room_id: room.room_id.clone(),
            viewer_secret: secret.into(),
            display_name: (!name.is_empty()).then(|| name.into()),
        },
    )
    .await;
    let ServerMessage::AuthSucceeded { peer_id, .. } = next_server(&mut socket).await else {
        panic!("viewer auth should succeed");
    };
    (socket, peer_id)
}

#[tokio::test]
async fn superseded_presenter_socket_cannot_act_or_disconnect_its_replacement() {
    let server = spawn_server().await;
    let room = create_public_room(&server, 3).await;
    let (mut old_presenter, old_peer_id, _) = authenticate_presenter(&server, &room).await;
    let (mut replacement, replacement_peer_id, _) = authenticate_presenter(&server, &room).await;
    assert_eq!(old_peer_id, replacement_peer_id);

    send_client(
        &mut old_presenter,
        ClientMessage::IceRefresh {
            protocol_version: PROTOCOL_VERSION,
            request_id: "stale-ice-refresh".into(),
        },
    )
    .await;
    assert!(matches!(
        next_server(&mut old_presenter).await,
        ServerMessage::Error {
            code: ErrorCode::AuthorizationDenied,
            ..
        }
    ));

    send_client(
        &mut old_presenter,
        ClientMessage::RoomClose {
            protocol_version: PROTOCOL_VERSION,
            request_id: "stale-close".into(),
        },
    )
    .await;
    assert!(matches!(
        next_server(&mut old_presenter).await,
        ServerMessage::Error {
            code: ErrorCode::AuthorizationDenied,
            ..
        }
    ));

    old_presenter.close(None).await.expect("close stale socket");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    send_client(
        &mut replacement,
        ClientMessage::RoomUpdateCapacity {
            protocol_version: PROTOCOL_VERSION,
            request_id: "replacement-capacity".into(),
            maximum_viewers: 2,
        },
    )
    .await;
    assert!(matches!(
        wait_for(&mut replacement, |message| matches!(
            message,
            ServerMessage::RoomCapacityUpdated {
                request_id,
                maximum_viewers: 2,
                ..
            } if request_id == "replacement-capacity"
        ))
        .await,
        ServerMessage::RoomCapacityUpdated {
            maximum_viewers: 2,
            ..
        }
    ));
}

#[tokio::test]
async fn health_readiness_room_creation_and_origin_policy() {
    let server = spawn_server().await;
    let client = Client::new();
    assert_eq!(
        client
            .get(format!("{}/healthz", server.base_url))
            .send()
            .await
            .expect("health")
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .get(format!("{}/readyz", server.base_url))
            .send()
            .await
            .expect("ready")
            .status(),
        StatusCode::OK
    );

    let room = create_public_room(&server, 4).await;
    assert_eq!(room.access_policy, RoomAccessPolicy::Public);
    let invitation = Url::parse(&room.viewer_url).expect("viewer URL");
    assert_eq!(invitation.path(), format!("/r/{}", room.room_id));
    assert_eq!(invitation.query(), Some("access=public"));
    assert!(
        invitation
            .fragment()
            .is_some_and(|fragment| fragment.len() >= 43)
    );
    assert!(!room.presenter_path.contains(&room.presenter_secret));

    let rejected = client
        .post(format!("{}/api/v1/rooms", server.base_url))
        .header("origin", "https://attacker.example")
        .json(&CreateRoomRequest {
            maximum_viewers: Some(4),
            expires_in_seconds: None,
            access_policy: None,
            allowed_friend_codes: None,
        })
        .send()
        .await
        .expect("origin rejection");
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    let api_missing = client
        .get(format!("{}/api/v1/not-found", server.base_url))
        .send()
        .await
        .expect("missing API");
    assert_eq!(api_missing.status(), StatusCode::NOT_FOUND);
    assert!(
        api_missing.headers()["content-type"]
            .to_str()
            .expect("content type")
            .starts_with("application/json")
    );
}

#[tokio::test]
async fn public_rooms_default_to_ten_viewers() {
    let server = spawn_server().await;
    let response = Client::new()
        .post(format!("{}/api/v1/rooms", server.base_url))
        .header("origin", &server.base_url)
        .json(&CreateRoomRequest {
            maximum_viewers: None,
            expires_in_seconds: Some(3_600),
            access_policy: Some(RoomAccessPolicy::Public),
            allowed_friend_codes: None,
        })
        .send()
        .await
        .expect("create default public room");
    assert_eq!(response.status(), StatusCode::CREATED);
    let room: CreateRoomResponse = response.json().await.expect("room response");
    assert_eq!(room.maximum_viewers, 10);
}

#[tokio::test]
async fn public_rooms_auto_admit_invited_viewers_and_reject_overflow() {
    let server = spawn_server().await;
    let room = create_public_room(&server, 1).await;
    let (mut presenter, presenter_id, _) = authenticate_presenter(&server, &room).await;
    let (mut viewer, viewer_id) = authenticate_viewer(&server, &room, "").await;

    let snapshot = wait_for(&mut presenter, |message| {
        matches!(message, ServerMessage::RoomSnapshot { snapshot, .. } if snapshot.approved_viewers.iter().any(|viewer| viewer.peer_id == viewer_id))
    })
    .await;
    assert!(
        matches!(snapshot, ServerMessage::RoomSnapshot { snapshot, .. } if snapshot.access_policy == RoomAccessPolicy::Public && snapshot.pending_viewers.is_empty())
    );

    send_client(
        &mut viewer,
        ClientMessage::ViewerUpdateDisplayName {
            protocol_version: PROTOCOL_VERSION,
            request_id: "rename-public-viewer".into(),
            display_name: Some("Public Viewer".into()),
        },
    )
    .await;
    assert!(matches!(
        next_server(&mut viewer).await,
        ServerMessage::ViewerDisplayNameUpdated { display_name: Some(name), .. }
            if name == "Public Viewer"
    ));
    let renamed = wait_for(&mut presenter, |message| {
        matches!(message, ServerMessage::RoomSnapshot { snapshot, .. }
            if snapshot.approved_viewers.iter().any(|viewer|
                viewer.peer_id == viewer_id && viewer.display_name.as_deref() == Some("Public Viewer")))
    })
    .await;
    assert!(matches!(renamed, ServerMessage::RoomSnapshot { .. }));

    send_client(
        &mut presenter,
        ClientMessage::SignalOffer {
            protocol_version: PROTOCOL_VERSION,
            request_id: "public-offer".into(),
            destination_peer_id: viewer_id.clone(),
            sdp: "public-offer-sdp".into(),
            ice_restart: false,
        },
    )
    .await;
    assert!(matches!(
        next_server(&mut viewer).await,
        ServerMessage::SignalOffer { source_peer_id, .. } if source_peer_id == presenter_id
    ));

    let invitation = Url::parse(&room.viewer_url).expect("viewer URL");
    let mut overflow = connect_websocket(&server).await;
    send_client(
        &mut overflow,
        ClientMessage::AuthViewer {
            protocol_version: PROTOCOL_VERSION,
            request_id: "overflow-auth".into(),
            room_id: room.room_id.clone(),
            viewer_secret: invitation.fragment().expect("fragment").into(),
            display_name: Some("Overflow".into()),
        },
    )
    .await;
    assert!(matches!(
        next_server(&mut overflow).await,
        ServerMessage::AuthFailed {
            code: ErrorCode::RoomFull,
            ..
        }
    ));
}

#[tokio::test]
async fn sharing_state_is_broadcast_persisted_and_presenter_authorized() {
    let server = spawn_server().await;
    let room = create_public_room(&server, 3).await;
    let (mut presenter, _, _) = authenticate_presenter(&server, &room).await;
    let (mut viewer, _) = authenticate_viewer(&server, &room, "First").await;

    send_client(
        &mut presenter,
        ClientMessage::RoomUpdateSharingState {
            protocol_version: PROTOCOL_VERSION,
            request_id: "pause".into(),
            sharing_state: SharingState::Paused,
        },
    )
    .await;
    assert!(matches!(
        wait_for(&mut presenter, |message| matches!(
            message,
            ServerMessage::RoomSharingStateUpdated {
                sharing_state: SharingState::Paused,
                ..
            }
        ))
        .await,
        ServerMessage::RoomSharingStateUpdated {
            sharing_state: SharingState::Paused,
            ..
        }
    ));
    assert!(matches!(
        wait_for(&mut viewer, |message| matches!(
            message,
            ServerMessage::RoomSharingStateUpdated {
                sharing_state: SharingState::Paused,
                ..
            }
        ))
        .await,
        ServerMessage::RoomSharingStateUpdated {
            sharing_state: SharingState::Paused,
            ..
        }
    ));

    let invitation = Url::parse(&room.viewer_url).expect("viewer URL");
    let mut late_viewer = connect_websocket(&server).await;
    send_client(
        &mut late_viewer,
        ClientMessage::AuthViewer {
            protocol_version: PROTOCOL_VERSION,
            request_id: "late-auth".into(),
            room_id: room.room_id.clone(),
            viewer_secret: invitation.fragment().expect("fragment").into(),
            display_name: Some("Late".into()),
        },
    )
    .await;
    assert!(matches!(
        next_server(&mut late_viewer).await,
        ServerMessage::AuthSucceeded { snapshot, .. }
            if snapshot.sharing_state == SharingState::Paused
    ));

    send_client(
        &mut viewer,
        ClientMessage::RoomUpdateSharingState {
            protocol_version: PROTOCOL_VERSION,
            request_id: "forbidden".into(),
            sharing_state: SharingState::Live,
        },
    )
    .await;
    assert!(matches!(
        wait_for(&mut viewer, |message| matches!(
            message,
            ServerMessage::Error {
                code: ErrorCode::AuthorizationDenied,
                ..
            }
        ))
        .await,
        ServerMessage::Error {
            code: ErrorCode::AuthorizationDenied,
            ..
        }
    ));
}

#[tokio::test]
async fn approval_routes_offer_answer_and_ice_only_between_authorized_peers() {
    let server = spawn_server().await;
    let room = create_room(&server, 1).await;
    let (mut presenter, presenter_id, _) = authenticate_presenter(&server, &room).await;
    let (mut viewer, viewer_id) = authenticate_viewer(&server, &room, "Viewer One").await;
    let pending = wait_for(&mut presenter, |message| {
        matches!(message, ServerMessage::ViewerPending { .. })
    })
    .await;
    assert!(matches!(pending, ServerMessage::ViewerPending { .. }));

    send_client(
        &mut presenter,
        ClientMessage::ViewerApprove {
            protocol_version: PROTOCOL_VERSION,
            request_id: "approve".into(),
            peer_id: viewer_id.clone(),
        },
    )
    .await;
    assert!(matches!(
        wait_for(&mut viewer, |message| matches!(
            message,
            ServerMessage::ViewerApproved { .. }
        ))
        .await,
        ServerMessage::ViewerApproved { .. }
    ));

    send_client(
        &mut presenter,
        ClientMessage::SignalOffer {
            protocol_version: PROTOCOL_VERSION,
            request_id: "offer".into(),
            destination_peer_id: viewer_id.clone(),
            sdp: "v=0\r\n".into(),
            ice_restart: false,
        },
    )
    .await;
    let offer = wait_for(&mut viewer, |message| {
        matches!(message, ServerMessage::SignalOffer { .. })
    })
    .await;
    assert!(
        matches!(offer, ServerMessage::SignalOffer { source_peer_id, .. } if source_peer_id == presenter_id)
    );

    send_client(
        &mut viewer,
        ClientMessage::SignalAnswer {
            protocol_version: PROTOCOL_VERSION,
            request_id: "answer".into(),
            destination_peer_id: presenter_id.clone(),
            sdp: "v=0\r\nanswer".into(),
        },
    )
    .await;
    assert!(matches!(
        wait_for(&mut presenter, |message| matches!(message, ServerMessage::SignalAnswer { .. })).await,
        ServerMessage::SignalAnswer { source_peer_id, .. } if source_peer_id == viewer_id
    ));

    send_client(
        &mut viewer,
        ClientMessage::SignalAnswer {
            protocol_version: PROTOCOL_VERSION,
            request_id: "impersonation".into(),
            destination_peer_id: viewer_id,
            sdp: "v=0".into(),
        },
    )
    .await;
    assert!(matches!(
        wait_for(&mut viewer, |message| matches!(
            message,
            ServerMessage::Error { .. }
        ))
        .await,
        ServerMessage::Error {
            code: ErrorCode::AuthorizationDenied,
            ..
        }
    ));
}

#[tokio::test]
async fn capacity_rejection_message_limits_and_presenter_resumption() {
    let server = spawn_server().await;
    let room = create_room(&server, 1).await;
    let (mut presenter, presenter_id, resume_token) = authenticate_presenter(&server, &room).await;
    let (mut first, first_id) = authenticate_viewer(&server, &room, "First").await;
    let _ = wait_for(&mut presenter, |message| {
        matches!(message, ServerMessage::ViewerPending { .. })
    })
    .await;
    send_client(
        &mut presenter,
        ClientMessage::ViewerApprove {
            protocol_version: PROTOCOL_VERSION,
            request_id: "approve-first".into(),
            peer_id: first_id,
        },
    )
    .await;
    let _ = wait_for(&mut first, |message| {
        matches!(message, ServerMessage::ViewerApproved { .. })
    })
    .await;

    let (mut second, second_id) = authenticate_viewer(&server, &room, "Second").await;
    let _ = wait_for(&mut presenter, |message| matches!(message, ServerMessage::ViewerPending { viewer, .. } if viewer.peer_id == second_id)).await;
    send_client(
        &mut presenter,
        ClientMessage::ViewerApprove {
            protocol_version: PROTOCOL_VERSION,
            request_id: "approve-second".into(),
            peer_id: second_id,
        },
    )
    .await;
    assert!(matches!(
        wait_for(&mut presenter, |message| matches!(
            message,
            ServerMessage::Error { .. }
        ))
        .await,
        ServerMessage::Error {
            code: ErrorCode::RoomFull,
            ..
        }
    ));

    send_client(
        &mut presenter,
        ClientMessage::SignalOffer {
            protocol_version: PROTOCOL_VERSION,
            request_id: "oversized".into(),
            destination_peer_id: "unknown".into(),
            sdp: "x".repeat(2_049),
            ice_restart: false,
        },
    )
    .await;
    assert!(matches!(
        wait_for(&mut presenter, |message| matches!(
            message,
            ServerMessage::Error { .. }
        ))
        .await,
        ServerMessage::Error {
            code: ErrorCode::MessageTooLarge,
            ..
        }
    ));

    presenter.close(None).await.expect("close presenter socket");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut resumed = connect_websocket(&server).await;
    send_client(
        &mut resumed,
        ClientMessage::SessionResume {
            protocol_version: PROTOCOL_VERSION,
            request_id: "resume".into(),
            room_id: room.room_id,
            resume_token,
        },
    )
    .await;
    assert!(matches!(
        next_server(&mut resumed).await,
        ServerMessage::AuthSucceeded { peer_id, .. } if peer_id == presenter_id
    ));
    second.close(None).await.expect("close second viewer");
}

#[tokio::test]
async fn malformed_json_unsupported_versions_and_authentication_timeout_are_structured() {
    let server = spawn_server().await;
    let mut malformed = connect_websocket(&server).await;
    malformed
        .send(Message::Text("{".into()))
        .await
        .expect("send malformed");
    assert!(matches!(
        next_server(&mut malformed).await,
        ServerMessage::AuthFailed {
            code: ErrorCode::InvalidMessage,
            ..
        }
    ));

    let room = create_room(&server, 1).await;
    let mut unsupported = connect_websocket(&server).await;
    send_client(
        &mut unsupported,
        ClientMessage::AuthPresenter {
            protocol_version: 999,
            request_id: "unsupported".into(),
            room_id: room.room_id,
            presenter_secret: room.presenter_secret,
        },
    )
    .await;
    assert!(matches!(
        next_server(&mut unsupported).await,
        ServerMessage::AuthFailed {
            code: ErrorCode::UnsupportedProtocolVersion,
            ..
        }
    ));

    let mut idle = connect_websocket(&server).await;
    assert!(matches!(
        next_server(&mut idle).await,
        ServerMessage::AuthFailed {
            code: ErrorCode::AuthenticationRequired,
            ..
        }
    ));
}

#[tokio::test]
async fn friends_only_rooms_admit_allowlisted_identities_and_reject_strangers() {
    let server = spawn_server().await;
    let friend = Identity::new();
    let stranger = Identity::new();
    let room = create_friends_room(&server, vec![friend.code()]).await;
    assert_eq!(room.access_policy, RoomAccessPolicy::FriendsOnly);
    let invitation = Url::parse(&room.viewer_url).expect("viewer URL");
    assert_eq!(invitation.query(), Some("access=friends"));

    let (mut presenter, _, _) = authenticate_presenter(&server, &room).await;

    // The allowlisted friend proves its key and joins approved immediately.
    let (_friend_socket, admitted) = join_friends_room(&server, &room, &friend, "Friend").await;
    let ServerMessage::AuthSucceeded { snapshot, peer_id, .. } = admitted else {
        panic!("allowlisted friend should be admitted, got {admitted:?}");
    };
    assert!(snapshot.pending_viewers.is_empty());
    assert_eq!(
        snapshot
            .approved_viewers
            .iter()
            .find(|viewer| viewer.peer_id == peer_id)
            .and_then(|viewer| viewer.friend_code.clone()),
        Some(friend.code())
    );

    // A proven identity that is not on the allowlist is rejected.
    let (_stranger_socket, rejected) =
        join_friends_room(&server, &room, &stranger, "Stranger").await;
    assert!(matches!(
        rejected,
        ServerMessage::AuthFailed {
            code: ErrorCode::AuthenticationFailed,
            ..
        }
    ));

    // The presenter's snapshot labels the admitted friend by code.
    let labelled = wait_for(&mut presenter, |message| {
        matches!(message, ServerMessage::RoomSnapshot { snapshot, .. }
            if snapshot.approved_viewers.iter().any(|viewer| viewer.friend_code.as_deref() == Some(&friend.code())))
    })
    .await;
    assert!(matches!(labelled, ServerMessage::RoomSnapshot { .. }));
}

#[tokio::test]
async fn friends_only_rooms_require_a_valid_allowlist() {
    let server = spawn_server().await;
    // `0` and `1` are outside the base32 alphabet, so this code can't parse.
    for allowed in [None, Some(vec![]), Some(vec!["clr-0000-1111".to_owned()])] {
        let response = Client::new()
            .post(format!("{}/api/v1/rooms", server.base_url))
            .header("origin", &server.base_url)
            .json(&CreateRoomRequest {
                maximum_viewers: Some(4),
                expires_in_seconds: Some(3_600),
                access_policy: Some(RoomAccessPolicy::FriendsOnly),
                allowed_friend_codes: allowed,
            })
            .send()
            .await
            .expect("create room request");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn late_resume_reports_session_expired_and_fresh_authentication_still_works() {
    let server = spawn_server().await;
    let room = create_public_room(&server, 4).await;
    let (_presenter, _, _) = authenticate_presenter(&server, &room).await;

    let invitation = Url::parse(&room.viewer_url).expect("viewer URL");
    let mut viewer = connect_websocket(&server).await;
    send_client(
        &mut viewer,
        ClientMessage::AuthViewer {
            protocol_version: PROTOCOL_VERSION,
            request_id: "expiring-viewer".into(),
            room_id: room.room_id.clone(),
            viewer_secret: invitation.fragment().expect("fragment").into(),
            display_name: Some("Expiring".into()),
        },
    )
    .await;
    let ServerMessage::AuthSucceeded { resume_token, .. } = next_server(&mut viewer).await else {
        panic!("viewer auth should succeed");
    };

    // Disconnect and outstay the 500 ms resume grace (the session itself is
    // retained a little longer, so the late resume is precisely identified).
    viewer.close(None).await.expect("close viewer socket");
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    let mut late = connect_websocket(&server).await;
    send_client(
        &mut late,
        ClientMessage::SessionResume {
            protocol_version: PROTOCOL_VERSION,
            request_id: "late-resume".into(),
            room_id: room.room_id.clone(),
            resume_token,
        },
    )
    .await;
    assert!(matches!(
        next_server(&mut late).await,
        ServerMessage::AuthFailed {
            code: ErrorCode::SessionExpired,
            ..
        }
    ));

    // Falling back to fresh viewer authentication still gets into the room.
    let (_socket, _peer) = authenticate_viewer(&server, &room, "Fresh").await;
}
