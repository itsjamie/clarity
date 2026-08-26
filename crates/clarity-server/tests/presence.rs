//! End-to-end presence: two identities connect to `/api/v1/presence`, prove
//! their keys, and — once mutually subscribed — see each other's online and
//! hosting state.

use std::{collections::HashSet, net::SocketAddr};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use clarity_core::{RoomActorConfig, TurnConfig};
use clarity_protocol::{
    ClientMessage, CreateRoomRequest, CreateRoomResponse, FriendPresence, HostedRoom,
    PROTOCOL_VERSION, PresenceServerMessage, RoomAccessPolicy, ServerMessage, SharingState,
};
use clarity_server::{AppConfig, AppState, build_router, config::Environment};
use futures_util::{SinkExt, StreamExt};
use ring::signature::{Ed25519KeyPair, KeyPair};
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

impl TestServer {
    /// The `host[:port]` identity signatures are bound to.
    fn host(&self) -> &str {
        self.base_url.trim_start_matches("http://")
    }
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
        room_actor: RoomActorConfig::default(),
        room_token_hmac_key: SecretString::from(
            "test-room-token-hmac-key-at-least-32-characters".to_owned(),
        ),
        resume_token_hmac_key: SecretString::from(
            "test-resume-token-hmac-key-at-least-32-characters".to_owned(),
        ),
        websocket_auth_timeout: std::time::Duration::from_millis(500),
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
            urls: vec!["stun:turn.example.test:3478".into()],
            shared_secret: SecretString::from(
                "test-turn-shared-secret-at-least-32-characters".to_owned(),
            ),
            credential_ttl: Duration::hours(1),
        },
    }
}

/// A throwaway signing identity for the test.
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

async fn connect_presence(server: &TestServer) -> ClientWebSocket {
    let url = server.base_url.replacen("http://", "ws://", 1) + "/api/v1/presence";
    let mut request = url.into_client_request().expect("request");
    request.headers_mut().insert(
        "origin",
        HeaderValue::from_str(&server.base_url).expect("origin"),
    );
    connect_async(request).await.expect("presence connect").0
}

async fn recv(socket: &mut ClientWebSocket) -> PresenceServerMessage {
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
            .await
            .expect("presence message timeout")
            .expect("socket open")
            .expect("valid frame");
        if let Message::Text(text) = frame {
            return serde_json::from_str(&text).expect("presence protocol message");
        }
    }
}

async fn send(socket: &mut ClientWebSocket, json: serde_json::Value) {
    socket
        .send(Message::Text(json.to_string().into()))
        .await
        .expect("send");
}

/// Completes the challenge/hello/ready handshake and returns the code the
/// server derived (which must equal the identity's own code). The signature
/// is bound to the presence context and the server's host, exactly as the
/// real clients sign.
async fn handshake(socket: &mut ClientWebSocket, identity: &Identity, server: &TestServer) -> String {
    let PresenceServerMessage::Challenge { nonce, .. } = recv(socket).await else {
        panic!("expected challenge first");
    };
    let payload = clarity_protocol::identity_challenge_payload(
        clarity_protocol::IDENTITY_CONTEXT_PRESENCE,
        server.host(),
        &nonce,
    );
    send(
        socket,
        serde_json::json!({
            "type": "presence:hello",
            "protocolVersion": PROTOCOL_VERSION,
            "publicKey": identity.public_key_b64(),
            "signature": identity.sign_b64(&payload),
        }),
    )
    .await;
    let PresenceServerMessage::Ready { code, .. } = recv(socket).await else {
        panic!("expected ready after hello");
    };
    code
}

async fn subscribe(socket: &mut ClientWebSocket, codes: Vec<String>) {
    send(
        socket,
        serde_json::json!({
            "type": "presence:subscribe",
            "protocolVersion": PROTOCOL_VERSION,
            "codes": codes,
        }),
    )
    .await;
}

async fn announce(
    socket: &mut ClientWebSocket,
    hosting: Option<HostedRoom>,
    presenter_secret: Option<&str>,
) {
    send(
        socket,
        serde_json::json!({
            "type": "presence:announce",
            "protocolVersion": PROTOCOL_VERSION,
            "hosting": hosting,
            "presenterSecret": presenter_secret,
        }),
    )
    .await;
}

async fn create_public_room(server: &TestServer) -> CreateRoomResponse {
    let response = reqwest::Client::new()
        .post(format!("{}/api/v1/rooms", server.base_url))
        .header("origin", &server.base_url)
        .json(&CreateRoomRequest {
            maximum_viewers: Some(4),
            expires_in_seconds: Some(3_600),
            access_policy: Some(RoomAccessPolicy::Public),
            allowed_friend_codes: None,
        })
        .send()
        .await
        .expect("create room request");
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    response.json().await.expect("room response")
}

async fn connect_room(server: &TestServer) -> ClientWebSocket {
    let url = server.base_url.replacen("http://", "ws://", 1) + "/api/v1/ws";
    let mut request = url.into_client_request().expect("request");
    request.headers_mut().insert(
        "origin",
        HeaderValue::from_str(&server.base_url).expect("origin"),
    );
    connect_async(request).await.expect("room connect").0
}

async fn send_room(socket: &mut ClientWebSocket, message: ClientMessage) {
    socket
        .send(Message::Text(
            serde_json::to_string(&message).expect("serialize").into(),
        ))
        .await
        .expect("send room message");
}

async fn recv_room(socket: &mut ClientWebSocket) -> ServerMessage {
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
            .await
            .expect("room message timeout")
            .expect("socket open")
            .expect("valid frame");
        if let Message::Text(text) = frame {
            let parsed: ServerMessage = serde_json::from_str(&text).expect("room protocol message");
            if let ServerMessage::HeartbeatPing { nonce, .. } = parsed {
                send_room(
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

async fn authenticate_presenter(
    server: &TestServer,
    room: &CreateRoomResponse,
) -> ClientWebSocket {
    let mut socket = connect_room(server).await;
    send_room(
        &mut socket,
        ClientMessage::AuthPresenter {
            protocol_version: PROTOCOL_VERSION,
            request_id: "presenter-auth".into(),
            room_id: room.room_id.clone(),
            presenter_secret: room.presenter_secret.clone(),
        },
    )
    .await;
    let ServerMessage::AuthSucceeded { .. } = recv_room(&mut socket).await else {
        panic!("presenter auth should succeed");
    };
    socket
}

async fn authenticate_viewer(server: &TestServer, room: &CreateRoomResponse) -> ClientWebSocket {
    let secret = Url::parse(&room.viewer_url)
        .expect("viewer URL")
        .fragment()
        .expect("fragment")
        .to_owned();
    let mut socket = connect_room(server).await;
    send_room(
        &mut socket,
        ClientMessage::AuthViewer {
            protocol_version: PROTOCOL_VERSION,
            request_id: "viewer-auth".into(),
            room_id: room.room_id.clone(),
            viewer_secret: secret,
            display_name: Some("Watcher".into()),
        },
    )
    .await;
    let ServerMessage::AuthSucceeded { .. } = recv_room(&mut socket).await else {
        panic!("viewer auth should succeed");
    };
    socket
}

/// Reads presence messages until a `presence:requests` set arrives, returning
/// its codes.
async fn wait_requests(socket: &mut ClientWebSocket) -> Vec<String> {
    loop {
        if let PresenceServerMessage::Requests { codes, .. } = recv(socket).await {
            return codes;
        }
    }
}

/// Reads presence messages until one reports a friend matching `predicate`.
async fn wait_presence(
    socket: &mut ClientWebSocket,
    predicate: impl Fn(&FriendPresence) -> bool,
) -> FriendPresence {
    loop {
        let friends = match recv(socket).await {
            PresenceServerMessage::Snapshot { friends, .. } => friends,
            PresenceServerMessage::Update { friend, .. } => vec![friend],
            _ => continue,
        };
        if let Some(found) = friends.into_iter().find(&predicate) {
            return found;
        }
    }
}

#[tokio::test]
async fn mutual_friends_see_presence_and_last_seen() {
    let server = spawn_server().await;
    let id_a = Identity::new();
    let id_b = Identity::new();

    let mut a = connect_presence(&server).await;
    let mut b = connect_presence(&server).await;
    assert_eq!(handshake(&mut a, &id_a, &server).await, id_a.code());
    assert_eq!(handshake(&mut b, &id_b, &server).await, id_b.code());

    // Mutually subscribe; each then sees the other online.
    subscribe(&mut a, vec![id_b.code()]).await;
    subscribe(&mut b, vec![id_a.code()]).await;
    let seen_b = wait_presence(&mut a, |f| f.code == id_b.code() && f.online).await;
    assert!(seen_b.hosting.is_none());
    assert_eq!(seen_b.last_seen_seconds_ago, None);
    wait_presence(&mut b, |f| f.code == id_a.code() && f.online).await;

    // A disconnects; B sees it go offline with a last-seen time.
    drop(a);
    let gone = wait_presence(&mut b, |f| f.code == id_a.code() && !f.online).await;
    assert!(gone.last_seen_seconds_ago.is_some());
}

#[tokio::test]
async fn a_one_sided_add_arrives_as_a_request_and_accepting_makes_it_mutual() {
    let server = spawn_server().await;
    let id_a = Identity::new();
    let id_b = Identity::new();

    // A adds B and goes away; the request outlives A's connection.
    let mut a = connect_presence(&server).await;
    handshake(&mut a, &id_a, &server).await;
    subscribe(&mut a, vec![id_b.code()]).await;
    assert_eq!(wait_requests(&mut a).await, Vec::<String>::new());
    drop(a);

    // B connects later and subscribes its (empty) contact set: the pending
    // request is in its very first requests set.
    let mut b = connect_presence(&server).await;
    handshake(&mut b, &id_b, &server).await;
    subscribe(&mut b, Vec::new()).await;
    assert_eq!(wait_requests(&mut b).await, vec![id_a.code()]);

    // B accepts by adding A back; its own pending set clears.
    subscribe(&mut b, vec![id_a.code()]).await;
    assert_eq!(wait_requests(&mut b).await, Vec::<String>::new());

    // A returns: the pair is mutual, so each sees the other's presence, and
    // neither side has a pending request. The snapshot precedes the requests
    // set on the wire, so read it first.
    let mut a = connect_presence(&server).await;
    handshake(&mut a, &id_a, &server).await;
    subscribe(&mut a, vec![id_b.code()]).await;
    wait_presence(&mut a, |f| f.code == id_b.code() && f.online).await;
    assert_eq!(wait_requests(&mut a).await, Vec::<String>::new());
    wait_presence(&mut b, |f| f.code == id_a.code() && f.online).await;
}

#[tokio::test]
async fn cancelling_an_invite_withdraws_the_request() {
    let server = spawn_server().await;
    let id_a = Identity::new();
    let id_b = Identity::new();

    let mut a = connect_presence(&server).await;
    let mut b = connect_presence(&server).await;
    handshake(&mut a, &id_a, &server).await;
    handshake(&mut b, &id_b, &server).await;
    subscribe(&mut b, Vec::new()).await;
    assert_eq!(wait_requests(&mut b).await, Vec::<String>::new());

    subscribe(&mut a, vec![id_b.code()]).await;
    assert_eq!(wait_requests(&mut b).await, vec![id_a.code()]);

    // A cancels: resubscribing without B withdraws the request live.
    subscribe(&mut a, Vec::new()).await;
    assert_eq!(wait_requests(&mut b).await, Vec::<String>::new());
}

#[tokio::test]
async fn hosting_is_validated_and_pushed_live_from_the_room() {
    let server = spawn_server().await;
    let id_a = Identity::new();
    let id_b = Identity::new();

    let mut a = connect_presence(&server).await;
    let mut b = connect_presence(&server).await;
    handshake(&mut a, &id_a, &server).await;
    handshake(&mut b, &id_b, &server).await;
    subscribe(&mut a, vec![id_b.code()]).await;
    subscribe(&mut b, vec![id_a.code()]).await;
    wait_presence(&mut b, |f| f.code == id_a.code() && f.online).await;

    // A room the registry does not know is never shown to friends.
    announce(
        &mut a,
        Some(HostedRoom {
            room_id: "no-such-room".to_owned(),
            viewer_url: format!("{}/r/no-such-room#secret", server.base_url),
            viewer_count: 9,
            sharing_state: SharingState::Live,
        }),
        Some("not-a-secret"),
    )
    .await;

    let room = create_public_room(&server).await;
    let mut presenter = authenticate_presenter(&server, &room).await;

    // A real room announced without the presenter secret (or with the wrong
    // one) is dropped: knowing a room id must not let anyone claim to host it.
    announce(
        &mut a,
        Some(HostedRoom {
            room_id: room.room_id.clone(),
            viewer_url: room.viewer_url.clone(),
            viewer_count: 42,
            sharing_state: SharingState::Live,
        }),
        Some("wrong-secret"),
    )
    .await;

    // A proven announcement pointing viewers away from this deployment is
    // dropped too: announced URLs render as links in every friend's client.
    announce(
        &mut a,
        Some(HostedRoom {
            room_id: room.room_id.clone(),
            viewer_url: "https://evil.example/r/x#secret".to_owned(),
            viewer_count: 42,
            sharing_state: SharingState::Live,
        }),
        Some(room.presenter_secret.as_str()),
    )
    .await;

    // Hosting a real room with the real secret: the announced viewer count is
    // replaced with the room actor's own.
    announce(
        &mut a,
        Some(HostedRoom {
            room_id: room.room_id.clone(),
            viewer_url: room.viewer_url.clone(),
            viewer_count: 42,
            sharing_state: SharingState::Live,
        }),
        Some(room.presenter_secret.as_str()),
    )
    .await;
    let seen = wait_presence(&mut b, |f| f.code == id_a.code() && f.hosting.is_some()).await;
    let hosting = seen.hosting.expect("hosting");
    // Had any of the rejected announces been applied, it would have arrived
    // first (with its asserted count of 9 or 42, or the foreign URL).
    assert_eq!(hosting.room_id, room.room_id);
    assert_eq!(hosting.viewer_url, room.viewer_url);
    assert_eq!(hosting.viewer_count, 0);
    assert_eq!(hosting.sharing_state, SharingState::Idle);

    // A viewer joins: the room pushes the new count without a re-announce.
    let mut viewer = authenticate_viewer(&server, &room).await;
    wait_presence(&mut b, |f| {
        f.code == id_a.code()
            && f.hosting.as_ref().is_some_and(|h| h.viewer_count == 1)
    })
    .await;

    // Sharing starts: the state change is pushed too.
    send_room(
        &mut presenter,
        ClientMessage::RoomUpdateSharingState {
            protocol_version: PROTOCOL_VERSION,
            request_id: "go-live".into(),
            sharing_state: SharingState::Live,
        },
    )
    .await;
    wait_presence(&mut b, |f| {
        f.code == id_a.code()
            && f.hosting
                .as_ref()
                .is_some_and(|h| h.sharing_state == SharingState::Live)
    })
    .await;

    // The viewer leaves: the count drops back.
    send_room(
        &mut viewer,
        ClientMessage::PeerLeave {
            protocol_version: PROTOCOL_VERSION,
            request_id: "leave".into(),
        },
    )
    .await;
    wait_presence(&mut b, |f| {
        f.code == id_a.code()
            && f.hosting.as_ref().is_some_and(|h| h.viewer_count == 0)
    })
    .await;

    // The presenter closes the room: friends see the hosting disappear while
    // A itself stays online.
    send_room(
        &mut presenter,
        ClientMessage::RoomClose {
            protocol_version: PROTOCOL_VERSION,
            request_id: "close".into(),
        },
    )
    .await;
    wait_presence(&mut b, |f| {
        f.code == id_a.code() && f.online && f.hosting.is_none()
    })
    .await;
}

#[tokio::test]
async fn a_bad_signature_is_rejected() {
    let server = spawn_server().await;
    let identity = Identity::new();
    let mut socket = connect_presence(&server).await;
    let PresenceServerMessage::Challenge { .. } = recv(&mut socket).await else {
        panic!("expected challenge");
    };
    // Sign the wrong bytes.
    send(
        &mut socket,
        serde_json::json!({
            "type": "presence:hello",
            "protocolVersion": PROTOCOL_VERSION,
            "publicKey": identity.public_key_b64(),
            "signature": identity.sign_b64("not the challenge"),
        }),
    )
    .await;
    assert!(matches!(
        recv(&mut socket).await,
        PresenceServerMessage::Error { .. }
    ));
}
