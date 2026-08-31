use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use clarity_protocol::{ClientMessage, ErrorCode, PROTOCOL_VERSION, ServerMessage};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::ORIGIN;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Signs a challenge with the local identity's private key, returning the
/// 64-byte Ed25519 signature. Injected so this crate needs no knowledge of how
/// identities are stored.
pub type Signer = Arc<dyn Fn(&[u8]) -> [u8; 64] + Send + Sync>;

/// Proof material for rooms that require an identity (friends-only rooms).
/// The signaling client answers the server's `auth:identity-challenge` with it
/// during authentication.
#[derive(Clone)]
pub struct SessionIdentity {
    /// The 32-byte Ed25519 public key.
    pub public_key: Vec<u8>,
    pub sign: Signer,
}

/// Mirrors the web client's signaling states so both clients describe
/// connection health in the same vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalingState {
    Connecting,
    Authenticating,
    Connected,
    Reconnecting,
    Closed,
    Failed,
}

#[derive(Debug)]
pub enum SignalingEvent {
    State(SignalingState),
    Message(Box<ServerMessage>),
}

#[derive(Clone)]
pub struct SignalingConfig {
    pub url: String,
    /// Origin header for the upgrade request; the server rejects upgrades
    /// without an allowlisted origin.
    pub origin: String,
    pub room_id: String,
    pub authentication: ClientMessage,
    /// Answers the identity challenge friends-only rooms issue during
    /// authentication. Without one, such rooms fail authentication.
    pub identity: Option<SessionIdentity>,
}

enum Command {
    Send(Box<ClientMessage>),
    Disconnect { send_leave: bool },
}

/// Owns the signaling WebSocket for one session: authentication, session
/// resume across reconnects, heartbeat replies, identity challenges, and
/// bounded exponential backoff. Server messages and state changes arrive on
/// the event channel; heartbeats and identity challenges are answered
/// internally and never surface.
///
/// A resume rejected by the server (grace window passed, or the peer already
/// pruned) falls back to the original authentication instead of failing, so a
/// session survives arbitrarily long outages as long as its credentials still
/// admit it.
///
/// Messages carrying user intent (approvals, kicks, room close, sharing
/// state, display name) sent while the connection is down are queued and
/// replayed once authentication completes. Reactive signaling traffic
/// (offers, answers, candidates) is dropped instead — the triggering server
/// message will not arrive again until after the session resumes.
pub struct SignalingClient {
    commands: mpsc::UnboundedSender<Command>,
}

impl SignalingClient {
    pub fn connect(config: SignalingConfig) -> (Self, mpsc::UnboundedReceiver<SignalingEvent>) {
        ensure_crypto_provider();
        let (events, event_receiver) = mpsc::unbounded_channel();
        let (commands, command_receiver) = mpsc::unbounded_channel();
        tokio::spawn(run(config, command_receiver, events));
        (Self { commands }, event_receiver)
    }

    pub fn send(&self, message: ClientMessage) {
        let _ = self.commands.send(Command::Send(Box::new(message)));
    }

    /// Ends the session permanently. With `send_leave`, announces departure so
    /// the server releases this peer immediately instead of waiting out the
    /// resume grace window.
    pub fn disconnect(&self, send_leave: bool) {
        let _ = self.commands.send(Command::Disconnect { send_leave });
    }
}

pub fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// How long before its expiry an ICE configuration is refreshed, so an ICE
/// restart or rebuild never gathers relay candidates with expired TURN
/// credentials.
const ICE_REFRESH_LEAD: time::Duration = time::Duration::seconds(60);

/// The retry cadence when the expiry is unreadable or a refresh went
/// unanswered.
const ICE_REFRESH_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// When to send `ice:refresh` for a configuration expiring at `expires_at`
/// (RFC 3339): [`ICE_REFRESH_LEAD`] before expiry, floored at a few seconds.
pub(crate) fn ice_refresh_delay(expires_at: &str) -> std::time::Duration {
    const MINIMUM: std::time::Duration = std::time::Duration::from_secs(5);
    let Ok(expiry) = time::OffsetDateTime::parse(
        expires_at,
        &time::format_description::well_known::Rfc3339,
    ) else {
        return ICE_REFRESH_RETRY;
    };
    let until = expiry - ICE_REFRESH_LEAD - time::OffsetDateTime::now_utc();
    std::time::Duration::try_from(until)
        .unwrap_or(MINIMUM)
        .max(MINIMUM)
}

/// A retry timer for a refresh that got no `ice:configuration` back.
pub(crate) fn ice_refresh_retry() -> std::time::Duration {
    ICE_REFRESH_RETRY
}

/// The `host[:port]` an identity challenge signature is bound to, from the
/// URL this client dialed. Default ports are omitted, matching the server's
/// canonicalization of its public base URL and allowed origins (see
/// [`clarity_protocol::identity_challenge_payload`]).
pub(crate) fn server_host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| crate::url_authority(&url))
        .unwrap_or_default()
}

/// rustls requires a process-level crypto provider before the first TLS
/// handshake and aborts the connection task with a panic otherwise. Installing
/// can lose the race to a host application that picked its own provider; that
/// is fine, so the result is ignored.
pub(crate) fn ensure_crypto_provider() {
    use std::sync::OnceLock;
    static INSTALL: OnceLock<()> = OnceLock::new();
    INSTALL.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum Outcome {
    Stopped {
        send_leave: bool,
    },
    ConnectionLost,
    /// The server rejected this connection's `session:resume` (expired grace
    /// window or a pruned peer); reconnect immediately with fresh credentials.
    ResumeRejected,
}

/// Whether a message carries user intent that must survive an outage, as
/// opposed to reactive signaling that is only meaningful in the moment.
fn is_user_intent(message: &ClientMessage) -> bool {
    matches!(
        message,
        ClientMessage::RoomClose { .. }
            | ClientMessage::RoomUpdateSharingState { .. }
            | ClientMessage::RoomUpdateCapacity { .. }
            | ClientMessage::ViewerUpdateDisplayName { .. }
            | ClientMessage::ViewerApprove { .. }
            | ClientMessage::ViewerReject { .. }
            | ClientMessage::ViewerKick { .. }
    )
}

async fn run(
    config: SignalingConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<SignalingEvent>,
) {
    let Ok(request) = build_request(&config) else {
        emit_state(&events, SignalingState::Failed);
        return;
    };
    let mut attempt: u32 = 0;
    let mut resume_token: Option<String> = None;
    let mut pending: VecDeque<ClientMessage> = VecDeque::new();
    emit_state(&events, SignalingState::Connecting);
    loop {
        if let Ok((mut socket, _)) = tokio_tungstenite::connect_async(request.clone()).await {
            emit_state(&events, SignalingState::Authenticating);
            let resuming = resume_token.is_some();
            let authentication = match &resume_token {
                Some(token) => ClientMessage::SessionResume {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: new_request_id(),
                    room_id: config.room_id.clone(),
                    resume_token: token.clone(),
                },
                None => config.authentication.clone(),
            };
            if send_message(&mut socket, &authentication).await.is_ok() {
                match drive(
                    &mut socket,
                    &mut commands,
                    &events,
                    &config,
                    resuming,
                    &mut resume_token,
                    &mut attempt,
                    &mut pending,
                )
                .await
                {
                    Outcome::Stopped { send_leave } => {
                        if send_leave {
                            let leave = ClientMessage::PeerLeave {
                                protocol_version: PROTOCOL_VERSION,
                                request_id: new_request_id(),
                            };
                            let _ = send_message(&mut socket, &leave).await;
                        }
                        let _ = socket.close(None).await;
                        emit_state(&events, SignalingState::Closed);
                        return;
                    }
                    Outcome::ConnectionLost => {}
                    Outcome::ResumeRejected => {
                        tracing::info!(
                            "the resumable session expired; authenticating from scratch"
                        );
                        resume_token = None;
                        continue;
                    }
                }
            }
        }
        emit_state(&events, SignalingState::Reconnecting);
        let delay = reconnect_delay(attempt, rand::random::<f64>());
        attempt = attempt.saturating_add(1);
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            command = commands.recv() => match command {
                None | Some(Command::Disconnect { .. }) => {
                    emit_state(&events, SignalingState::Closed);
                    return;
                }
                Some(Command::Send(message)) => {
                    if is_user_intent(&message) {
                        pending.push_back(*message);
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn drive(
    socket: &mut Socket,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &mpsc::UnboundedSender<SignalingEvent>,
    config: &SignalingConfig,
    resuming: bool,
    resume_token: &mut Option<String>,
    attempt: &mut u32,
    pending: &mut VecDeque<ClientMessage>,
) -> Outcome {
    let mut authenticated = false;
    // Reactive traffic produced before this connection finishes
    // authenticating. The server treats anything sent mid-authentication as
    // the authentication reply, so everything is held back until
    // `auth:succeeded`; these are dropped if the connection is lost first.
    let mut held: Vec<ClientMessage> = Vec::new();
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None => return Outcome::Stopped { send_leave: false },
                Some(Command::Disconnect { send_leave }) => return Outcome::Stopped { send_leave },
                Some(Command::Send(message)) => {
                    if !authenticated {
                        if is_user_intent(&message) {
                            pending.push_back(*message);
                        } else {
                            held.push(*message);
                        }
                    } else if send_message(socket, &message).await.is_err() {
                        if is_user_intent(&message) {
                            pending.push_front(*message);
                        }
                        return Outcome::ConnectionLost;
                    }
                }
            },
            frame = socket.next() => {
                let Some(Ok(frame)) = frame else {
                    return Outcome::ConnectionLost;
                };
                let Message::Text(text) = frame else {
                    continue;
                };
                let Ok(message) = serde_json::from_str::<ServerMessage>(text.as_str()) else {
                    tracing::warn!("received a message that does not match the protocol");
                    return Outcome::ConnectionLost;
                };
                match &message {
                    ServerMessage::HeartbeatPing { nonce, .. } => {
                        let pong = ClientMessage::HeartbeatPong {
                            protocol_version: PROTOCOL_VERSION,
                            nonce: nonce.clone(),
                        };
                        if send_message(socket, &pong).await.is_err() {
                            return Outcome::ConnectionLost;
                        }
                        continue;
                    }
                    ServerMessage::AuthIdentityChallenge { nonce, .. } => {
                        // Answered internally, like heartbeats; the whole
                        // exchange must finish within the server's auth
                        // timeout, so signing happens right here.
                        let Some(identity) = &config.identity else {
                            tracing::warn!(
                                "the room requires a proven identity but none was configured"
                            );
                            continue;
                        };
                        let payload = clarity_protocol::identity_challenge_payload(
                            clarity_protocol::IDENTITY_CONTEXT_ROOM_AUTH,
                            &server_host(&config.url),
                            nonce,
                        );
                        let reply = ClientMessage::AuthIdentity {
                            protocol_version: PROTOCOL_VERSION,
                            request_id: new_request_id(),
                            public_key: BASE64.encode(&identity.public_key),
                            signature: BASE64.encode((identity.sign)(payload.as_bytes())),
                        };
                        if send_message(socket, &reply).await.is_err() {
                            return Outcome::ConnectionLost;
                        }
                        continue;
                    }
                    ServerMessage::AuthSucceeded { resume_token: token, .. } => {
                        *resume_token = Some(token.clone());
                        *attempt = 0;
                        authenticated = true;
                        emit_state(events, SignalingState::Connected);
                        // Replay intent queued during the outage, then the
                        // reactive traffic held during authentication.
                        while let Some(queued) = pending.pop_front() {
                            if send_message(socket, &queued).await.is_err() {
                                pending.push_front(queued);
                                return Outcome::ConnectionLost;
                            }
                        }
                        for message in held.drain(..) {
                            if send_message(socket, &message).await.is_err() {
                                return Outcome::ConnectionLost;
                            }
                        }
                    }
                    ServerMessage::AuthFailed { .. } => {
                        if resuming && !authenticated {
                            return Outcome::ResumeRejected;
                        }
                        *resume_token = None;
                        emit_state(events, SignalingState::Failed);
                    }
                    ServerMessage::Error { code: ErrorCode::SessionExpired, .. }
                        if resuming && !authenticated =>
                    {
                        return Outcome::ResumeRejected;
                    }
                    _ => {}
                }
                let _ = events.send(SignalingEvent::Message(Box::new(message)));
            }
        }
    }
}

async fn send_message(
    socket: &mut Socket,
    message: &ClientMessage,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let json = serde_json::to_string(message).expect("client messages always serialize");
    socket.send(Message::text(json)).await
}

fn build_request(config: &SignalingConfig) -> Result<Request, ()> {
    let mut request = config.url.as_str().into_client_request().map_err(|_| ())?;
    let origin = HeaderValue::from_str(&config.origin).map_err(|_| ())?;
    request.headers_mut().insert(ORIGIN, origin);
    Ok(request)
}

fn emit_state(events: &mpsc::UnboundedSender<SignalingEvent>, state: SignalingState) {
    let _ = events.send(SignalingEvent::State(state));
}

/// Same backoff curve as the web client: 500ms doubling to a 10s cap, scaled
/// by ±25% jitter.
pub(crate) fn reconnect_delay(attempt: u32, jitter: f64) -> Duration {
    let base = (500_u64 * 2_u64.pow(attempt.min(5))).min(10_000);
    let jitter = jitter.clamp(0.0, 1.0);
    Duration::from_millis((base as f64 * (0.75 + jitter * 0.5)).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_delay_matches_the_web_backoff_curve() {
        assert_eq!(reconnect_delay(0, 0.0), Duration::from_millis(375));
        assert_eq!(reconnect_delay(0, 1.0), Duration::from_millis(625));
        assert_eq!(reconnect_delay(3, 0.5), Duration::from_millis(4000));
        assert_eq!(reconnect_delay(5, 0.5), Duration::from_millis(10_000));
        assert_eq!(reconnect_delay(50, 0.5), Duration::from_millis(10_000));
        assert_eq!(reconnect_delay(50, 9.0), Duration::from_millis(12_500));
    }

    #[test]
    fn identity_host_preserves_ipv6_brackets() {
        assert_eq!(
            server_host("wss://[2001:db8::1]:8443/api/v1/ws"),
            "[2001:db8::1]:8443"
        );
    }

    #[test]
    fn user_intent_messages_are_queued_reactive_signaling_is_not() {
        let intent = ClientMessage::ViewerApprove {
            protocol_version: PROTOCOL_VERSION,
            request_id: new_request_id(),
            peer_id: "peer".into(),
        };
        assert!(is_user_intent(&intent));
        let reactive = ClientMessage::SignalIceRestart {
            protocol_version: PROTOCOL_VERSION,
            request_id: new_request_id(),
            destination_peer_id: "peer".into(),
        };
        assert!(!is_user_intent(&reactive));
    }
}
