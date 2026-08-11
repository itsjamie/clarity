//! The presence WebSocket: `/api/v1/presence`.
//!
//! Unlike the room signaling socket, a presence connection is authenticated by
//! *identity*, not by a room secret. The server issues a random challenge, the
//! client signs it with its Ed25519 key, and the derived friend code becomes the
//! connection's identity in the presence registry. From there the client
//! subscribes to its contacts and announces what it is sharing.

use std::net::SocketAddr;

use axum::{
    extract::{
        ConnectInfo, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use clarity_core::{RoomCommand, new_challenge, verify_identity_for_hosts};
use clarity_protocol::{
    ErrorCode, HostedRoom, IDENTITY_CONTEXT_PRESENCE, PROTOCOL_VERSION, PresenceClientMessage,
    PresenceServerMessage, RoomLifecycle,
};
use secrecy::SecretString;
use url::Url;
use futures_util::{SinkExt, StreamExt};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

use crate::{
    AppState,
    app::{AppError, validate_origin},
    rate_limit::SessionRateLimiter,
};

pub async fn upgrade(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> Result<Response, AppError> {
    validate_origin(&state.config, &headers)?;
    if !state.rate_limits.check(
        "presence-connect",
        &remote.ip().to_string(),
        state.config.websocket_connection_rate_limit,
    ) {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::RateLimited,
            "Too many presence connection attempts.",
        ));
    }
    let maximum_size = state.config.websocket_max_message_bytes;
    Ok(websocket
        .max_message_size(maximum_size)
        .max_frame_size(maximum_size)
        .on_upgrade(move |socket| handle_presence(socket, state))
        .into_response())
}

async fn handle_presence(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let (outbound_tx, mut outbound_rx) =
        mpsc::channel::<PresenceServerMessage>(state.config.room_actor.outbound_capacity);

    // The writer owns the sink: it serializes outbound presence messages and
    // pings the client on an interval so a dead peer can be detected by the
    // absence of any returning frame (the reader's read deadline).
    let ping_interval = state.config.websocket_heartbeat_interval;
    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(ping_interval);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                message = outbound_rx.recv() => {
                    let Some(message) = message else { break };
                    let Ok(json) = serde_json::to_string(&message) else { break };
                    if sink.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });

    // Challenge → Hello → Ready handshake.
    let challenge = new_challenge();
    if outbound_tx
        .try_send(PresenceServerMessage::Challenge {
            protocol_version: PROTOCOL_VERSION,
            server_timestamp: now_string(),
            nonce: challenge.clone(),
        })
        .is_err()
    {
        finish(outbound_tx, writer).await;
        return;
    }

    let read_deadline = state.config.websocket_heartbeat_interval + state.config.websocket_heartbeat_timeout;
    let identity_hosts = state.config.identity_hosts();
    let code = match authenticate(
        &mut stream,
        &challenge,
        &identity_hosts,
        state.config.websocket_auth_timeout,
    )
    .await
    {
        Ok(code) => code,
        Err(reason) => {
            let _ = outbound_tx.try_send(presence_error(reason));
            finish(outbound_tx, writer).await;
            return;
        }
    };

    let _ = outbound_tx.try_send(PresenceServerMessage::Ready {
        protocol_version: PROTOCOL_VERSION,
        server_timestamp: now_string(),
        code: code.clone(),
    });
    let session_id = state.presence.connect(code.clone(), outbound_tx.clone());
    debug!(%code, "presence session authenticated");

    let mut limiter = SessionRateLimiter::per_minute(state.config.signal_rate_limit);
    loop {
        let next = tokio::time::timeout(read_deadline, stream.next()).await;
        let frame = match next {
            Err(_) => break, // no frame within the read deadline: the peer is gone
            Ok(frame) => frame,
        };
        let Some(Ok(message)) = frame else { break };
        let text = match message {
            Message::Text(text) => text,
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) | Message::Binary(_) => break,
        };
        if text.len() > state.config.websocket_max_message_bytes {
            break;
        }
        let Ok(parsed) = serde_json::from_str::<PresenceClientMessage>(&text) else {
            let _ = outbound_tx.try_send(presence_error(Reject::Malformed));
            continue;
        };
        if !limiter.check() {
            let _ = outbound_tx.try_send(presence_error(Reject::RateLimited));
            continue;
        }
        match parsed {
            PresenceClientMessage::Subscribe { codes, .. } => {
                state.presence.subscribe(session_id, codes);
            }
            PresenceClientMessage::Announce {
                hosting,
                presenter_secret,
                ..
            } => match hosting {
                None => state.presence.announce(session_id, None),
                Some(room) => {
                    if let Some(hosting) =
                        authoritative_hosting(&state, room, presenter_secret).await
                    {
                        state.presence.announce(session_id, Some(hosting));
                    }
                    // An unknown or closed room, an unproven announcer, or a
                    // viewer URL pointing away from this deployment cannot be
                    // announced; the claim is dropped rather than shown to
                    // friends.
                }
            },
            // A second Hello on an established session is ignored.
            PresenceClientMessage::Hello { .. } => {}
        }
    }

    state.presence.disconnect(session_id);
    debug!(%code, "presence session disconnected");
    finish(outbound_tx, writer).await;
}

/// Validates a hosting announcement before it reaches any friend: the
/// announcer must prove presenter-ship with the room's presenter secret, the
/// viewer URL must point at this deployment, and the client-asserted viewer
/// count and sharing state are replaced with the room actor's authoritative
/// values. Returns `None` when any check fails, so the claim is ignored.
async fn authoritative_hosting(
    state: &AppState,
    room: HostedRoom,
    presenter_secret: Option<String>,
) -> Option<HostedRoom> {
    if !viewer_url_is_local(&state.config, &room.viewer_url) {
        debug!(room_id = %room.room_id, "dropping an announcement with a foreign viewer URL");
        return None;
    }
    let credential = SecretString::from(presenter_secret?);
    let (verify_tx, verify_rx) = oneshot::channel();
    state
        .registry
        .dispatch(
            &room.room_id,
            RoomCommand::VerifyPresenter {
                credential,
                reply: verify_tx,
            },
        )
        .await
        .ok()?;
    if !verify_rx.await.ok()? {
        debug!(room_id = %room.room_id, "dropping an announcement that failed the presenter proof");
        return None;
    }
    let (reply_tx, reply_rx) = oneshot::channel();
    state
        .registry
        .dispatch(&room.room_id, RoomCommand::Snapshot { reply: reply_tx })
        .await
        .ok()?;
    let snapshot = reply_rx.await.ok()?;
    if snapshot.lifecycle != RoomLifecycle::Open {
        return None;
    }
    Some(HostedRoom {
        viewer_count: u32::try_from(snapshot.approved_viewers.len()).unwrap_or(u32::MAX),
        sharing_state: snapshot.sharing_state,
        ..room
    })
}

/// Whether a viewer URL points at this deployment: an `http`/`https` URL
/// whose origin is the public base URL or one of the allowed origins.
/// Announced URLs are rendered as links in every friend's client, so
/// anything else (`javascript:`, an attacker's domain) is rejected.
fn viewer_url_is_local(config: &crate::config::AppConfig, viewer_url: &str) -> bool {
    let Ok(url) = Url::parse(viewer_url) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let origin = url.origin().ascii_serialization();
    origin == config.public_base_url.origin().ascii_serialization()
        || config.allowed_origins.contains(&origin)
}

/// Reads the first message, which must be a valid `Hello`, and returns the
/// friend code proven by its domain-separated signature over `challenge`.
async fn authenticate(
    stream: &mut (impl StreamExt<Item = Result<Message, axum::Error>> + Unpin),
    challenge: &str,
    identity_hosts: &[String],
    timeout: std::time::Duration,
) -> Result<String, Reject> {
    let first = tokio::time::timeout(timeout, stream.next())
        .await
        .map_err(|_| Reject::Timeout)?;
    let Some(Ok(Message::Text(text))) = first else {
        return Err(Reject::Malformed);
    };
    let message =
        serde_json::from_str::<PresenceClientMessage>(&text).map_err(|_| Reject::Malformed)?;
    let PresenceClientMessage::Hello {
        protocol_version,
        public_key,
        signature,
    } = message
    else {
        return Err(Reject::NotHello);
    };
    if protocol_version != PROTOCOL_VERSION {
        return Err(Reject::UnsupportedVersion);
    }
    verify_identity_for_hosts(
        &public_key,
        &signature,
        IDENTITY_CONTEXT_PRESENCE,
        identity_hosts,
        challenge,
    )
    .map_err(|_| Reject::BadSignature)
}

#[derive(Clone, Copy)]
enum Reject {
    Timeout,
    Malformed,
    NotHello,
    UnsupportedVersion,
    BadSignature,
    RateLimited,
}

fn presence_error(reason: Reject) -> PresenceServerMessage {
    let (code, message) = match reason {
        Reject::Timeout => (
            ErrorCode::AuthenticationRequired,
            "The presence handshake was not completed in time.",
        ),
        Reject::Malformed => (ErrorCode::InvalidMessage, "The presence message was not valid."),
        Reject::NotHello => (
            ErrorCode::AuthenticationRequired,
            "The first presence message must be a hello.",
        ),
        Reject::UnsupportedVersion => (
            ErrorCode::UnsupportedProtocolVersion,
            "This client protocol version is not supported.",
        ),
        Reject::BadSignature => (
            ErrorCode::AuthenticationFailed,
            "The presence identity could not be verified.",
        ),
        Reject::RateLimited => (
            ErrorCode::RateLimited,
            "The presence message rate limit was exceeded.",
        ),
    };
    PresenceServerMessage::Error {
        protocol_version: PROTOCOL_VERSION,
        server_timestamp: now_string(),
        code,
        message: message.to_owned(),
    }
}

async fn finish(
    outbound: mpsc::Sender<PresenceServerMessage>,
    mut writer: tokio::task::JoinHandle<()>,
) {
    drop(outbound);
    if tokio::time::timeout(std::time::Duration::from_millis(250), &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

fn now_string() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}
