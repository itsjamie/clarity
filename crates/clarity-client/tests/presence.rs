//! The client presence session against a real server: two identities that add
//! each other end up seeing each other online.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;

use clarity_client::presence::{PresenceConfig, PresenceEvent, PresenceSession};
use clarity_core::{RoomActorConfig, TurnConfig};
use clarity_identity::Identity;
use clarity_server::{AppConfig, AppState, build_router, config::Environment};
use secrecy::SecretString;
use time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedReceiver;
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

async fn spawn_server() -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let app = build_router(AppState::new(test_config(address)))
        .into_make_service_with_connect_info::<SocketAddr>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer {
        http_base: format!("http://{address}"),
        task,
    }
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

fn presence_config(server: &TestServer, identity: &Identity) -> PresenceConfig {
    let signer = identity.clone();
    PresenceConfig {
        url: server.http_base.replacen("http://", "ws://", 1) + "/api/v1/presence",
        origin: server.http_base.clone(),
        public_key: identity.public_key().to_vec(),
        sign: Arc::new(move |message: &[u8]| signer.sign(message)),
    }
}

/// Waits until a friend `code` is reported online.
async fn wait_online(events: &mut UnboundedReceiver<PresenceEvent>, code: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.recv()).await {
        let friends = match event {
            PresenceEvent::Snapshot(friends) => friends,
            PresenceEvent::Update(friend) => vec![friend],
            _ => continue,
        };
        if friends.iter().any(|f| f.code == code && f.online) {
            return;
        }
    }
    panic!("did not see {code} come online");
}

#[tokio::test]
async fn mutually_added_sessions_see_each_other_online() {
    let server = spawn_server().await;
    let a = Identity::create("A", "device").expect("identity");
    let b = Identity::create("B", "device").expect("identity");

    let (session_a, mut events_a) = PresenceSession::connect(presence_config(&server, &a));
    let (session_b, mut events_b) = PresenceSession::connect(presence_config(&server, &b));

    session_a.subscribe(vec![b.friend_code()]);
    session_b.subscribe(vec![a.friend_code()]);

    wait_online(&mut events_a, &b.friend_code()).await;
    wait_online(&mut events_b, &a.friend_code()).await;

    session_a.stop();
    session_b.stop();
}
