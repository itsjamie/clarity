//! Exercises the signaling client against an in-process WebSocket server:
//! authentication, heartbeat replies, resume-token reconnection, and the
//! Origin header the real server requires.

use std::time::Duration;

use clarity_client::signaling::{SignalingClient, SignalingConfig, SignalingEvent, SignalingState};
use clarity_protocol::{
    ClientMessage, IceConfiguration, PROTOCOL_VERSION, PeerRole, RoomAccessPolicy, RoomLifecycle,
    RoomSnapshot, ServerMessage, SharingState,
};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

const TIMEOUT: Duration = Duration::from_secs(5);

// The Err variant's size is fixed by tungstenite's header-callback signature.
#[allow(clippy::result_large_err)]
async fn accept_asserting_origin(
    listener: &TcpListener,
    expected_origin: &str,
) -> WebSocketStream<TcpStream> {
    let (stream, _) = listener.accept().await.expect("accepts a connection");
    let expected = expected_origin.to_owned();
    tokio_tungstenite::accept_hdr_async(stream, move |request: &Request, response: Response| {
        let origin = request
            .headers()
            .get("origin")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(origin, expected, "upgrade must carry the allowed origin");
        Ok(response)
    })
    .await
    .expect("upgrades the connection")
}

async fn read_client_message(socket: &mut WebSocketStream<TcpStream>) -> ClientMessage {
    loop {
        let frame = tokio::time::timeout(TIMEOUT, socket.next())
            .await
            .expect("client sends in time")
            .expect("socket stays open")
            .expect("frame is readable");
        if let Message::Text(text) = frame {
            return serde_json::from_str(text.as_str()).expect("client speaks the protocol");
        }
    }
}

async fn send_server_message(socket: &mut WebSocketStream<TcpStream>, message: &ServerMessage) {
    socket
        .send(Message::text(
            serde_json::to_string(message).expect("serializes"),
        ))
        .await
        .expect("server can send");
}

async fn next_event(events: &mut UnboundedReceiver<SignalingEvent>) -> SignalingEvent {
    tokio::time::timeout(TIMEOUT, events.recv())
        .await
        .expect("an event arrives in time")
        .expect("the event channel stays open")
}

async fn expect_state(events: &mut UnboundedReceiver<SignalingEvent>, expected: SignalingState) {
    match next_event(events).await {
        SignalingEvent::State(state) => assert_eq!(state, expected),
        SignalingEvent::Message(message) => {
            panic!("expected state {expected:?}, received message {message:?}")
        }
    }
}

fn auth_succeeded(resume_token: &str, request_id: &str) -> ServerMessage {
    ServerMessage::AuthSucceeded {
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        server_timestamp: "2026-01-01T00:00:00Z".into(),
        room_id: "room-1".into(),
        peer_id: "peer-1".into(),
        role: PeerRole::Viewer,
        resume_token: resume_token.to_owned(),
        resume_expires_at: "2026-01-01T01:00:00Z".into(),
        snapshot: RoomSnapshot {
            room_id: "room-1".into(),
            lifecycle: RoomLifecycle::Open,
            sharing_state: SharingState::Idle,
            access_policy: RoomAccessPolicy::Public,
            maximum_viewers: 10,
            expires_at: "2026-01-01T01:00:00Z".into(),
            expires_in_seconds: 3_600,
            presenter_connected: true,
            pending_viewers: vec![],
            approved_viewers: vec![],
        },
        ice_configuration: IceConfiguration {
            expires_at: "2026-01-01T01:00:00Z".into(),
            ice_servers: vec![],
        },
    }
}

/// A `wss://` attempt must reach TLS negotiation and fail like any other
/// connection failure. Before the client installed a rustls crypto provider,
/// this panicked the signaling task instead, closing the event channel.
#[tokio::test]
async fn wss_connections_survive_tls_setup_and_schedule_reconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let port = listener.local_addr().expect("has an address").port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });

    let (client, mut events) = SignalingClient::connect(SignalingConfig {
        url: format!("wss://127.0.0.1:{port}/api/v1/ws"),
        origin: format!("https://127.0.0.1:{port}"),
        room_id: "room-1".into(),
        authentication: ClientMessage::AuthViewer {
            protocol_version: PROTOCOL_VERSION,
            request_id: "auth-1".into(),
            room_id: "room-1".into(),
            viewer_secret: "secret".into(),
            display_name: None,
        },
        identity: None,
    });

    expect_state(&mut events, SignalingState::Connecting).await;
    expect_state(&mut events, SignalingState::Reconnecting).await;
    client.disconnect(false);
}

#[tokio::test]
async fn authenticates_heartbeats_resumes_and_leaves() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let port = listener.local_addr().expect("has an address").port();
    let origin = format!("http://127.0.0.1:{port}");

    let (client, mut events) = SignalingClient::connect(SignalingConfig {
        url: format!("ws://127.0.0.1:{port}/api/v1/ws"),
        origin: origin.clone(),
        room_id: "room-1".into(),
        authentication: ClientMessage::AuthViewer {
            protocol_version: PROTOCOL_VERSION,
            request_id: "auth-1".into(),
            room_id: "room-1".into(),
            viewer_secret: "secret".into(),
            display_name: Some("Native".into()),
        },
        identity: None,
    });

    // First connection: fresh viewer authentication.
    let mut socket = accept_asserting_origin(&listener, &origin).await;
    expect_state(&mut events, SignalingState::Connecting).await;
    expect_state(&mut events, SignalingState::Authenticating).await;
    let auth = read_client_message(&mut socket).await;
    assert!(matches!(
        auth,
        ClientMessage::AuthViewer { ref viewer_secret, .. } if viewer_secret == "secret"
    ));
    send_server_message(&mut socket, &auth_succeeded("resume-1", "auth-1")).await;
    expect_state(&mut events, SignalingState::Connected).await;
    assert!(matches!(
        next_event(&mut events).await,
        SignalingEvent::Message(message) if matches!(*message, ServerMessage::AuthSucceeded { .. })
    ));

    // Heartbeats are answered internally and never surface as events.
    send_server_message(
        &mut socket,
        &ServerMessage::HeartbeatPing {
            protocol_version: PROTOCOL_VERSION,
            server_timestamp: "2026-01-01T00:00:01Z".into(),
            nonce: "nonce-1".into(),
        },
    )
    .await;
    assert!(matches!(
        read_client_message(&mut socket).await,
        ClientMessage::HeartbeatPong { ref nonce, .. } if nonce == "nonce-1"
    ));

    // Dropping the connection triggers resume with the stored token.
    drop(socket);
    expect_state(&mut events, SignalingState::Reconnecting).await;
    let mut socket = accept_asserting_origin(&listener, &origin).await;
    expect_state(&mut events, SignalingState::Authenticating).await;
    assert!(matches!(
        read_client_message(&mut socket).await,
        ClientMessage::SessionResume { ref resume_token, .. } if resume_token == "resume-1"
    ));
    send_server_message(&mut socket, &auth_succeeded("resume-2", "resume-req")).await;
    expect_state(&mut events, SignalingState::Connected).await;
    assert!(matches!(
        next_event(&mut events).await,
        SignalingEvent::Message(message) if matches!(*message, ServerMessage::AuthSucceeded { .. })
    ));

    // Graceful disconnect announces the departure before closing.
    client.disconnect(true);
    assert!(matches!(
        read_client_message(&mut socket).await,
        ClientMessage::PeerLeave { .. }
    ));
    expect_state(&mut events, SignalingState::Closed).await;
}

/// A resume the server rejects (grace window over) must fall back to the
/// original credentials on a fresh connection instead of failing the session,
/// and user intent sent during the outage must be replayed after the fresh
/// authentication.
#[tokio::test]
async fn rejected_resume_falls_back_to_fresh_auth_and_replays_intent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let port = listener.local_addr().expect("has an address").port();
    let origin = format!("http://127.0.0.1:{port}");

    let (client, mut events) = SignalingClient::connect(SignalingConfig {
        url: format!("ws://127.0.0.1:{port}/api/v1/ws"),
        origin: origin.clone(),
        room_id: "room-1".into(),
        authentication: ClientMessage::AuthPresenter {
            protocol_version: PROTOCOL_VERSION,
            request_id: "auth-1".into(),
            room_id: "room-1".into(),
            presenter_secret: "secret".into(),
        },
        identity: None,
    });

    let mut socket = accept_asserting_origin(&listener, &origin).await;
    expect_state(&mut events, SignalingState::Connecting).await;
    expect_state(&mut events, SignalingState::Authenticating).await;
    assert!(matches!(
        read_client_message(&mut socket).await,
        ClientMessage::AuthPresenter { .. }
    ));
    send_server_message(&mut socket, &auth_succeeded("resume-1", "auth-1")).await;
    expect_state(&mut events, SignalingState::Connected).await;
    assert!(matches!(
        next_event(&mut events).await,
        SignalingEvent::Message(message) if matches!(*message, ServerMessage::AuthSucceeded { .. })
    ));

    // The connection drops; intent sent while offline must not be lost.
    drop(socket);
    expect_state(&mut events, SignalingState::Reconnecting).await;
    client.send(ClientMessage::ViewerApprove {
        protocol_version: PROTOCOL_VERSION,
        request_id: "approve-1".into(),
        peer_id: "viewer-1".into(),
    });

    // The resume attempt is rejected: the grace window has passed.
    let mut socket = accept_asserting_origin(&listener, &origin).await;
    expect_state(&mut events, SignalingState::Authenticating).await;
    assert!(matches!(
        read_client_message(&mut socket).await,
        ClientMessage::SessionResume { .. }
    ));
    send_server_message(
        &mut socket,
        &ServerMessage::AuthFailed {
            protocol_version: PROTOCOL_VERSION,
            request_id: "resume-req".into(),
            server_timestamp: "2026-01-01T00:01:00Z".into(),
            code: clarity_protocol::ErrorCode::SessionExpired,
            message: "The resumable session has expired; authenticate again.".into(),
        },
    )
    .await;
    drop(socket);

    // Fallback: a fresh connection carrying the original authentication, and
    // the queued approval replayed right after it succeeds.
    let mut socket = accept_asserting_origin(&listener, &origin).await;
    expect_state(&mut events, SignalingState::Authenticating).await;
    assert!(matches!(
        read_client_message(&mut socket).await,
        ClientMessage::AuthPresenter { ref presenter_secret, .. } if presenter_secret == "secret"
    ));
    send_server_message(&mut socket, &auth_succeeded("resume-2", "auth-1")).await;
    expect_state(&mut events, SignalingState::Connected).await;
    assert!(matches!(
        next_event(&mut events).await,
        SignalingEvent::Message(message) if matches!(*message, ServerMessage::AuthSucceeded { .. })
    ));
    assert!(matches!(
        read_client_message(&mut socket).await,
        ClientMessage::ViewerApprove { ref peer_id, .. } if peer_id == "viewer-1"
    ));

    client.disconnect(false);
    expect_state(&mut events, SignalingState::Closed).await;
}
