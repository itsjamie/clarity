//! Client half of the presence channel.
//!
//! [`PresenceSession`] owns a resilient connection to `/api/v1/presence`: it
//! signs the server's challenge to prove its identity, keeps its contact
//! subscription and hosting announcement applied across reconnects, and streams
//! friends' presence back as [`PresenceEvent`]s. Signing is injected so this
//! crate needs no knowledge of how identities are stored.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use clarity_protocol::{
    FriendPresence, HostedRoom, PROTOCOL_VERSION, PresenceClientMessage, PresenceServerMessage,
};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::ORIGIN;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use secrecy::{ExposeSecret, SecretString};

use crate::signaling::{ensure_crypto_provider, reconnect_delay, server_host};

pub use crate::signaling::Signer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceState {
    Connecting,
    Connected,
    Reconnecting,
    Closed,
}

#[derive(Debug, Clone)]
pub enum PresenceEvent {
    State(PresenceState),
    /// The friend code the server derived from this identity's key.
    Ready {
        code: String,
    },
    /// The full set of currently-visible friends, on (re)subscribe.
    Snapshot(Vec<FriendPresence>),
    /// One friend's presence changed.
    Update(FriendPresence),
    /// The full set of codes that added this identity and are waiting for it
    /// to add them back — its incoming friend requests. Replaces any previous
    /// set.
    Requests(Vec<String>),
}

pub struct PresenceConfig {
    /// WebSocket URL of the presence endpoint, e.g. `ws://host/api/v1/presence`.
    pub url: String,
    pub origin: String,
    /// The 32-byte Ed25519 public key.
    pub public_key: Vec<u8>,
    pub sign: Signer,
}

/// A hosting announcement: the room to show friends plus the presenter secret
/// that proves this session hosts it. The secret goes only to the server,
/// which drops unproven announcements; it is never forwarded to friends.
#[derive(Clone)]
pub struct HostingAnnouncement {
    pub room: HostedRoom,
    pub presenter_secret: SecretString,
}

enum Command {
    Subscribe(Vec<String>),
    Announce(Option<HostingAnnouncement>),
    Stop,
}

/// Handle to a running presence connection. Dropping it does not stop the
/// session; call [`stop`](Self::stop).
pub struct PresenceSession {
    commands: mpsc::UnboundedSender<Command>,
}

impl PresenceSession {
    pub fn connect(config: PresenceConfig) -> (Self, mpsc::UnboundedReceiver<PresenceEvent>) {
        ensure_crypto_provider();
        let (events, event_receiver) = mpsc::unbounded_channel();
        let (commands, command_receiver) = mpsc::unbounded_channel();
        tokio::spawn(run(config, command_receiver, events));
        (Self { commands }, event_receiver)
    }

    /// Replaces the watched set of friend codes (typically the contact list).
    pub fn subscribe(&self, codes: Vec<String>) {
        let _ = self.commands.send(Command::Subscribe(codes));
    }

    /// Announces the room being hosted now, or `None` when not sharing.
    pub fn announce(&self, hosting: Option<HostingAnnouncement>) {
        let _ = self.commands.send(Command::Announce(hosting));
    }

    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum Outcome {
    Stopped,
    ConnectionLost,
}

/// State carried across reconnects so a fresh connection re-establishes the
/// same subscription and hosting announcement.
struct Applied {
    codes: Vec<String>,
    hosting: Option<HostingAnnouncement>,
}

async fn run(
    config: PresenceConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<PresenceEvent>,
) {
    let Ok(request) = build_request(&config) else {
        emit(&events, PresenceState::Closed);
        return;
    };
    let public_key = BASE64.encode(&config.public_key);
    let mut applied = Applied {
        codes: Vec::new(),
        hosting: None,
    };
    let mut attempt: u32 = 0;
    emit(&events, PresenceState::Connecting);
    loop {
        if let Ok((mut socket, _)) = tokio_tungstenite::connect_async(request.clone()).await {
            match session(
                &mut socket,
                &config,
                &public_key,
                &mut applied,
                &mut commands,
                &events,
            )
            .await
            {
                Outcome::Stopped => {
                    let _ = socket.close(None).await;
                    emit(&events, PresenceState::Closed);
                    return;
                }
                Outcome::ConnectionLost => {}
            }
        }
        emit(&events, PresenceState::Reconnecting);
        let delay = reconnect_delay(attempt, rand::random::<f64>());
        attempt = attempt.saturating_add(1);
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            command = commands.recv() => match command {
                None | Some(Command::Stop) => {
                    emit(&events, PresenceState::Closed);
                    return;
                }
                // Keep the intended state current while offline; it is applied
                // on the next successful connection.
                Some(Command::Subscribe(codes)) => applied.codes = codes,
                Some(Command::Announce(hosting)) => applied.hosting = hosting,
            }
        }
    }
}

/// One connection's lifetime: handshake, re-apply intent, then pump.
async fn session(
    socket: &mut Socket,
    config: &PresenceConfig,
    public_key: &str,
    applied: &mut Applied,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &mpsc::UnboundedSender<PresenceEvent>,
) -> Outcome {
    // Challenge → Hello → Ready.
    let Some(PresenceServerMessage::Challenge { nonce, .. }) = recv(socket).await else {
        return Outcome::ConnectionLost;
    };
    let payload = clarity_protocol::identity_challenge_payload(
        clarity_protocol::IDENTITY_CONTEXT_PRESENCE,
        &server_host(&config.url),
        &nonce,
    );
    let signature = BASE64.encode((config.sign)(payload.as_bytes()));
    let hello = PresenceClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        public_key: public_key.to_owned(),
        signature,
    };
    if send(socket, &hello).await.is_err() {
        return Outcome::ConnectionLost;
    }
    match recv(socket).await {
        Some(PresenceServerMessage::Ready { code, .. }) => {
            let _ = events.send(PresenceEvent::Ready { code });
        }
        _ => return Outcome::ConnectionLost,
    }
    emit(events, PresenceState::Connected);

    // Re-apply the current intent for this fresh connection.
    if send(socket, &subscribe_message(&applied.codes))
        .await
        .is_err()
    {
        return Outcome::ConnectionLost;
    }
    if applied.hosting.is_some()
        && send(socket, &announce_message(applied.hosting.clone()))
            .await
            .is_err()
    {
        return Outcome::ConnectionLost;
    }

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Stop) => return Outcome::Stopped,
                Some(Command::Subscribe(codes)) => {
                    applied.codes = codes;
                    if send(socket, &subscribe_message(&applied.codes)).await.is_err() {
                        return Outcome::ConnectionLost;
                    }
                }
                Some(Command::Announce(hosting)) => {
                    applied.hosting = hosting;
                    if send(socket, &announce_message(applied.hosting.clone())).await.is_err() {
                        return Outcome::ConnectionLost;
                    }
                }
            },
            frame = socket.next() => {
                let Some(Ok(frame)) = frame else {
                    return Outcome::ConnectionLost;
                };
                let Message::Text(text) = frame else {
                    continue; // ping/pong/binary
                };
                match serde_json::from_str::<PresenceServerMessage>(&text) {
                    Ok(PresenceServerMessage::Snapshot { friends, .. }) => {
                        let _ = events.send(PresenceEvent::Snapshot(friends));
                    }
                    Ok(PresenceServerMessage::Update { friend, .. }) => {
                        let _ = events.send(PresenceEvent::Update(friend));
                    }
                    Ok(PresenceServerMessage::Requests { codes, .. }) => {
                        let _ = events.send(PresenceEvent::Requests(codes));
                    }
                    Ok(_) => {}
                    Err(_) => return Outcome::ConnectionLost,
                }
            }
        }
    }
}

async fn recv(socket: &mut Socket) -> Option<PresenceServerMessage> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(text.as_str()).ok();
            }
            Some(Ok(_)) => continue, // ping/pong/binary
            _ => return None,
        }
    }
}

async fn send(
    socket: &mut Socket,
    message: &PresenceClientMessage,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let json = serde_json::to_string(message).expect("presence messages always serialize");
    socket.send(Message::text(json)).await
}

fn subscribe_message(codes: &[String]) -> PresenceClientMessage {
    PresenceClientMessage::Subscribe {
        protocol_version: PROTOCOL_VERSION,
        codes: codes.to_vec(),
    }
}

fn announce_message(hosting: Option<HostingAnnouncement>) -> PresenceClientMessage {
    let (hosting, presenter_secret) = match hosting {
        Some(announcement) => (
            Some(announcement.room),
            Some(announcement.presenter_secret.expose_secret().to_owned()),
        ),
        None => (None, None),
    };
    PresenceClientMessage::Announce {
        protocol_version: PROTOCOL_VERSION,
        hosting,
        presenter_secret,
    }
}

fn build_request(config: &PresenceConfig) -> Result<Request, ()> {
    let mut request = config.url.as_str().into_client_request().map_err(|_| ())?;
    let origin = HeaderValue::from_str(&config.origin).map_err(|_| ())?;
    request.headers_mut().insert(ORIGIN, origin);
    Ok(request)
}

fn emit(events: &mpsc::UnboundedSender<PresenceEvent>, state: PresenceState) {
    let _ = events.send(PresenceEvent::State(state));
}
