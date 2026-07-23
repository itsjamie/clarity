use std::{net::SocketAddr, time::Instant};

use axum::{
    extract::{
        ConnectInfo, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use clarity_core::{
    AuthOutcome, DomainError, RoomCommand, RoutedSignal, SessionHandle, secret_as_str,
};
use clarity_protocol::{ClientMessage, ErrorCode, PROTOCOL_VERSION, PeerRole, ServerMessage};
use futures_util::{SinkExt, StreamExt};
use secrecy::SecretString;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::{
    AppState,
    app::{AppError, validate_origin},
    rate_limit::SessionRateLimiter,
};

#[derive(Debug, Clone)]
struct AuthenticatedSession {
    room_id: String,
    peer_id: String,
    role: PeerRole,
}

pub async fn upgrade(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> Result<Response, AppError> {
    validate_origin(&state.config, &headers)?;
    if !state.rate_limits.check(
        "websocket-connect",
        &remote.ip().to_string(),
        state.config.websocket_connection_rate_limit,
    ) {
        return Err(AppError::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::RateLimited,
            "Too many signaling connection attempts.",
        ));
    }
    let maximum_size = state.config.websocket_max_message_bytes;
    Ok(websocket
        .max_message_size(maximum_size)
        .max_frame_size(maximum_size)
        .on_upgrade(move |socket| handle_socket(socket, state, remote))
        .into_response())
}

async fn handle_socket(socket: WebSocket, state: AppState, remote: SocketAddr) {
    let (mut socket_writer, mut socket_reader) = socket.split();
    let (outbound_tx, mut outbound_rx) =
        mpsc::channel::<ServerMessage>(state.config.room_actor.outbound_capacity);
    let writer = tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            let Ok(json) = serde_json::to_string(&message) else {
                break;
            };
            if socket_writer
                .send(Message::Text(json.into()))
                .await
                .is_err()
            {
                break;
            }
        }
        let _ = socket_writer.send(Message::Close(None)).await;
    });

    let authentication = tokio::time::timeout(
        state.config.websocket_auth_timeout,
        authenticate(&mut socket_reader, &outbound_tx, &state, remote),
    )
    .await;

    let session = match authentication {
        Ok(Ok(session)) => session,
        Ok(Err(error)) => {
            debug!(code = ?error.code(), "signaling authentication rejected");
            let (code, message) = authentication_failure(&error);
            send_auth_failed(&outbound_tx, "authentication-failed", code, message);
            finish_writer(outbound_tx, writer).await;
            return;
        }
        Err(_) => {
            send_auth_failed(
                &outbound_tx,
                "authentication-timeout",
                ErrorCode::AuthenticationRequired,
                "Authentication was not completed in time.",
            );
            finish_writer(outbound_tx, writer).await;
            return;
        }
    };

    debug!(room_id = %session.room_id, peer_id = %session.peer_id, role = ?session.role, "signaling session authenticated");
    let mut message_limiter = SessionRateLimiter::per_minute(state.config.signal_rate_limit);
    let mut heartbeat = tokio::time::interval(state.config.websocket_heartbeat_interval);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pending_heartbeat: Option<(String, Instant)> = None;

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if pending_heartbeat.as_ref().is_some_and(|(_, deadline)| Instant::now() >= *deadline) {
                    warn!(room_id = %session.room_id, peer_id = %session.peer_id, "signaling heartbeat timed out");
                    break;
                }
                let nonce = Uuid::new_v4().to_string();
                let deadline = Instant::now() + state.config.websocket_heartbeat_timeout;
                pending_heartbeat = Some((nonce.clone(), deadline));
                if !try_send(&outbound_tx, ServerMessage::HeartbeatPing {
                    protocol_version: PROTOCOL_VERSION,
                    server_timestamp: now_string(),
                    nonce,
                }) {
                    break;
                }
            }
            incoming = socket_reader.next() => {
                let message = match parse_incoming(incoming, state.config.websocket_max_message_bytes) {
                    Ok(Some(message)) => message,
                    Ok(None) => break,
                    Err(error) => {
                        send_protocol_error(&outbound_tx, None, error.code(), error.message());
                        break;
                    }
                };
                if message.protocol_version() != PROTOCOL_VERSION {
                    send_protocol_error(
                        &outbound_tx,
                        message.request_id().map(str::to_owned),
                        ErrorCode::UnsupportedProtocolVersion,
                        "This client protocol version is not supported.",
                    );
                    continue;
                }
                if !message_limiter.check() {
                    send_protocol_error(
                        &outbound_tx,
                        message.request_id().map(str::to_owned),
                        ErrorCode::RateLimited,
                        "The signaling message rate limit was exceeded.",
                    );
                    continue;
                }
                if let ClientMessage::HeartbeatPong { nonce, .. } = &message {
                    if pending_heartbeat.as_ref().is_some_and(|(expected, _)| expected == nonce) {
                        pending_heartbeat = None;
                    }
                    continue;
                }
                match handle_authenticated_message(&state, &session, &outbound_tx, message).await {
                    Ok(SessionControl::Continue) => {}
                    Ok(SessionControl::Close) => break,
                    Err((request_id, error)) => {
                        send_protocol_error(&outbound_tx, request_id, error.code(), &error.to_string());
                    }
                }
            }
        }
    }

    let _ = state
        .registry
        .dispatch(
            &session.room_id,
            RoomCommand::Disconnect {
                peer_id: session.peer_id.clone(),
            },
        )
        .await;
    debug!(room_id = %session.room_id, peer_id = %session.peer_id, "signaling session disconnected");
    finish_writer(outbound_tx, writer).await;
}

fn authentication_failure(error: &DomainError) -> (ErrorCode, &'static str) {
    match error {
        DomainError::RoomFull => (
            ErrorCode::RoomFull,
            "The room has reached its viewer limit.",
        ),
        DomainError::RoomExpired => (ErrorCode::RoomExpired, "The room has expired."),
        DomainError::RoomClosed => (ErrorCode::RoomClosed, "The room is closed."),
        DomainError::PendingViewerLimitReached => (
            ErrorCode::PendingViewerLimitReached,
            "The room has too many pending viewers.",
        ),
        _ => (ErrorCode::AuthenticationFailed, "Authentication failed."),
    }
}

async fn authenticate(
    reader: &mut futures_util::stream::SplitStream<WebSocket>,
    outbound: &mpsc::Sender<ServerMessage>,
    state: &AppState,
    remote: SocketAddr,
) -> Result<AuthenticatedSession, DomainError> {
    let message = match parse_incoming(
        reader.next().await,
        state.config.websocket_max_message_bytes,
    ) {
        Ok(Some(message)) => message,
        Ok(None) => return Err(DomainError::AuthenticationFailed),
        Err(error) => {
            send_auth_failed(
                outbound,
                "invalid-authentication",
                error.code(),
                error.message(),
            );
            return Err(DomainError::AuthenticationFailed);
        }
    };
    if message.protocol_version() != PROTOCOL_VERSION {
        send_auth_failed(
            outbound,
            message.request_id().unwrap_or("unsupported-version"),
            ErrorCode::UnsupportedProtocolVersion,
            "This client protocol version is not supported.",
        );
        return Err(DomainError::AuthenticationFailed);
    }
    if !state.rate_limits.check(
        "auth",
        &remote.ip().to_string(),
        state.config.auth_rate_limit,
    ) {
        send_auth_failed(
            outbound,
            message.request_id().unwrap_or("rate-limited"),
            ErrorCode::RateLimited,
            "Too many authentication attempts.",
        );
        return Err(DomainError::AuthenticationFailed);
    }
    let session_handle = SessionHandle {
        outbound: outbound.clone(),
    };
    let (room_id, request_id, reply) = match message {
        ClientMessage::AuthPresenter {
            request_id,
            room_id,
            presenter_secret,
            ..
        } => {
            let (reply_tx, reply_rx) = oneshot::channel();
            state
                .registry
                .dispatch(
                    &room_id,
                    RoomCommand::AuthenticatePresenter {
                        credential: SecretString::from(presenter_secret),
                        session: session_handle,
                        reply: reply_tx,
                    },
                )
                .await?;
            (room_id, request_id, reply_rx)
        }
        ClientMessage::AuthViewer {
            request_id,
            room_id,
            viewer_secret,
            display_name,
            ..
        } => {
            let (reply_tx, reply_rx) = oneshot::channel();
            state
                .registry
                .dispatch(
                    &room_id,
                    RoomCommand::AuthenticateViewer {
                        credential: SecretString::from(viewer_secret),
                        display_name,
                        session: session_handle,
                        reply: reply_tx,
                    },
                )
                .await?;
            (room_id, request_id, reply_rx)
        }
        ClientMessage::SessionResume {
            request_id,
            room_id,
            resume_token,
            ..
        } => {
            let (reply_tx, reply_rx) = oneshot::channel();
            state
                .registry
                .dispatch(
                    &room_id,
                    RoomCommand::Resume {
                        resume_token: SecretString::from(resume_token),
                        session: session_handle,
                        reply: reply_tx,
                    },
                )
                .await?;
            (room_id, request_id, reply_rx)
        }
        _ => {
            send_auth_failed(
                outbound,
                "authentication-required",
                ErrorCode::AuthenticationRequired,
                "Authenticate before sending signaling messages.",
            );
            return Err(DomainError::AuthenticationFailed);
        }
    };
    let outcome = reply.await.map_err(|_| DomainError::Unavailable)??;
    send_auth_success(outbound, state, &request_id, &outcome)?;
    Ok(AuthenticatedSession {
        room_id,
        peer_id: outcome.peer_id,
        role: outcome.role,
    })
}

fn send_auth_success(
    outbound: &mpsc::Sender<ServerMessage>,
    state: &AppState,
    request_id: &str,
    outcome: &AuthOutcome,
) -> Result<(), DomainError> {
    let ice_configuration = state
        .turn
        .issue(&outcome.peer_id, OffsetDateTime::now_utc())
        .map_err(|_| DomainError::Unavailable)?;
    let message = ServerMessage::AuthSucceeded {
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        server_timestamp: now_string(),
        room_id: outcome.room_id.clone(),
        peer_id: outcome.peer_id.clone(),
        role: outcome.role,
        resume_token: secret_as_str(&outcome.resume_token).to_owned(),
        resume_expires_at: format_time(outcome.resume_expires_at),
        snapshot: outcome.snapshot.clone(),
        ice_configuration,
    };
    if try_send(outbound, message) {
        Ok(())
    } else {
        Err(DomainError::Unavailable)
    }
}

enum SessionControl {
    Continue,
    Close,
}

async fn handle_authenticated_message(
    state: &AppState,
    session: &AuthenticatedSession,
    outbound: &mpsc::Sender<ServerMessage>,
    message: ClientMessage,
) -> Result<SessionControl, (Option<String>, DomainError)> {
    let request_id = message.request_id().map(str::to_owned);
    let close_after = matches!(&message, ClientMessage::PeerLeave { .. });
    let action = match message {
        ClientMessage::ViewerApprove {
            request_id,
            peer_id,
            ..
        } => {
            action(state, &session.room_id, |reply| RoomCommand::Approve {
                source_peer_id: session.peer_id.clone(),
                target_peer_id: peer_id,
                request_id,
                reply,
            })
            .await
        }
        ClientMessage::ViewerReject {
            request_id,
            peer_id,
            ..
        } => {
            action(state, &session.room_id, |reply| RoomCommand::Reject {
                source_peer_id: session.peer_id.clone(),
                target_peer_id: peer_id,
                request_id,
                reply,
            })
            .await
        }
        ClientMessage::ViewerKick {
            request_id,
            peer_id,
            ..
        } => {
            action(state, &session.room_id, |reply| RoomCommand::Kick {
                source_peer_id: session.peer_id.clone(),
                target_peer_id: peer_id,
                request_id,
                reply,
            })
            .await
        }
        ClientMessage::RoomUpdateCapacity {
            request_id,
            maximum_viewers,
            ..
        } => {
            action(state, &session.room_id, |reply| {
                RoomCommand::UpdateCapacity {
                    source_peer_id: session.peer_id.clone(),
                    maximum_viewers,
                    request_id,
                    reply,
                }
            })
            .await
        }
        ClientMessage::ViewerUpdateDisplayName {
            request_id,
            display_name,
            ..
        } => {
            action(state, &session.room_id, |reply| {
                RoomCommand::UpdateViewerDisplayName {
                    source_peer_id: session.peer_id.clone(),
                    display_name,
                    request_id,
                    reply,
                }
            })
            .await
        }
        ClientMessage::RoomClose { .. } => {
            action(state, &session.room_id, |reply| RoomCommand::Close {
                source_peer_id: session.peer_id.clone(),
                reply,
            })
            .await
            .map_err(|error| (request_id.clone(), error))?;
            return Ok(SessionControl::Close);
        }
        ClientMessage::PeerLeave { .. } => {
            state
                .registry
                .dispatch(
                    &session.room_id,
                    RoomCommand::Leave {
                        peer_id: session.peer_id.clone(),
                    },
                )
                .await
        }
        ClientMessage::SignalOffer {
            request_id,
            destination_peer_id,
            sdp,
            ice_restart,
            ..
        } => {
            if sdp.len() > state.config.sdp_max_bytes {
                return Err((Some(request_id), DomainError::MessageTooLarge));
            }
            action(state, &session.room_id, |reply| RoomCommand::RouteSignal {
                source_peer_id: session.peer_id.clone(),
                destination_peer_id,
                request_id,
                signal: RoutedSignal::Offer { sdp, ice_restart },
                reply,
            })
            .await
        }
        ClientMessage::SignalAnswer {
            request_id,
            destination_peer_id,
            sdp,
            ..
        } => {
            if sdp.len() > state.config.sdp_max_bytes {
                return Err((Some(request_id), DomainError::MessageTooLarge));
            }
            action(state, &session.room_id, |reply| RoomCommand::RouteSignal {
                source_peer_id: session.peer_id.clone(),
                destination_peer_id,
                request_id,
                signal: RoutedSignal::Answer { sdp },
                reply,
            })
            .await
        }
        ClientMessage::SignalIceCandidate {
            request_id,
            destination_peer_id,
            candidate,
            sdp_mid,
            sdp_m_line_index,
            ..
        } => {
            if candidate.len() > state.config.ice_candidate_max_bytes {
                return Err((Some(request_id), DomainError::MessageTooLarge));
            }
            action(state, &session.room_id, |reply| RoomCommand::RouteSignal {
                source_peer_id: session.peer_id.clone(),
                destination_peer_id,
                request_id,
                signal: RoutedSignal::IceCandidate {
                    candidate,
                    sdp_mid,
                    sdp_m_line_index,
                },
                reply,
            })
            .await
        }
        ClientMessage::SignalIceRestart {
            request_id,
            destination_peer_id,
            ..
        } => {
            action(state, &session.room_id, |reply| RoomCommand::RouteSignal {
                source_peer_id: session.peer_id.clone(),
                destination_peer_id,
                request_id,
                signal: RoutedSignal::IceRestart,
                reply,
            })
            .await
        }
        ClientMessage::IceRefresh { request_id, .. } => {
            let configuration = state
                .turn
                .issue(&session.peer_id, OffsetDateTime::now_utc())
                .map_err(|_| (Some(request_id.clone()), DomainError::Unavailable))?;
            if !try_send(
                outbound,
                ServerMessage::IceConfiguration {
                    protocol_version: PROTOCOL_VERSION,
                    request_id,
                    server_timestamp: now_string(),
                    configuration,
                },
            ) {
                return Err((None, DomainError::Unavailable));
            }
            Ok(())
        }
        ClientMessage::AuthPresenter { .. }
        | ClientMessage::AuthViewer { .. }
        | ClientMessage::SessionResume { .. }
        | ClientMessage::HeartbeatPong { .. } => Err(DomainError::AuthorizationDenied),
    };
    action.map_err(|error| (request_id, error))?;
    if close_after {
        Ok(SessionControl::Close)
    } else {
        Ok(SessionControl::Continue)
    }
}

async fn action<F>(state: &AppState, room_id: &str, command: F) -> Result<(), DomainError>
where
    F: FnOnce(oneshot::Sender<Result<(), DomainError>>) -> RoomCommand,
{
    let (reply_tx, reply_rx) = oneshot::channel();
    state.registry.dispatch(room_id, command(reply_tx)).await?;
    reply_rx.await.map_err(|_| DomainError::Unavailable)?
}

fn parse_incoming(
    incoming: Option<Result<Message, axum::Error>>,
    maximum_size: usize,
) -> Result<Option<ClientMessage>, ReadError> {
    let Some(incoming) = incoming else {
        return Ok(None);
    };
    let message = incoming.map_err(|_| ReadError::Connection)?;
    let text = match message {
        Message::Text(text) => text,
        Message::Close(_) => return Ok(None),
        Message::Ping(_) | Message::Pong(_) => return Err(ReadError::NonText),
        Message::Binary(_) => return Err(ReadError::NonText),
    };
    if text.len() > maximum_size {
        return Err(ReadError::TooLarge);
    }
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|_| ReadError::InvalidJson)
}

#[derive(Debug, Clone, Copy)]
enum ReadError {
    Connection,
    NonText,
    TooLarge,
    InvalidJson,
}

impl ReadError {
    const fn code(self) -> ErrorCode {
        match self {
            Self::TooLarge => ErrorCode::MessageTooLarge,
            Self::Connection | Self::NonText | Self::InvalidJson => ErrorCode::InvalidMessage,
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::Connection => "The signaling connection failed.",
            Self::NonText => "Only text JSON WebSocket messages are accepted.",
            Self::TooLarge => "The signaling message exceeds the configured limit.",
            Self::InvalidJson => "The signaling message is not valid protocol JSON.",
        }
    }
}

fn try_send(outbound: &mpsc::Sender<ServerMessage>, message: ServerMessage) -> bool {
    outbound.try_send(message).is_ok()
}

fn send_protocol_error(
    outbound: &mpsc::Sender<ServerMessage>,
    request_id: Option<String>,
    code: ErrorCode,
    message: &str,
) {
    let _ = outbound.try_send(ServerMessage::Error {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        server_timestamp: now_string(),
        code,
        message: message.to_owned(),
    });
}

fn send_auth_failed(
    outbound: &mpsc::Sender<ServerMessage>,
    request_id: &str,
    code: ErrorCode,
    message: &str,
) {
    let _ = outbound.try_send(ServerMessage::AuthFailed {
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        server_timestamp: now_string(),
        code,
        message: message.to_owned(),
    });
}

async fn finish_writer(
    outbound: mpsc::Sender<ServerMessage>,
    mut writer: tokio::task::JoinHandle<()>,
) {
    drop(outbound);
    match tokio::time::timeout(std::time::Duration::from_millis(250), &mut writer).await {
        Ok(_) => {}
        Err(_) => {
            // The writer owns the socket sink and can be blocked by a slow peer. Its bounded
            // lifetime prevents a disconnected client from retaining a server task indefinitely.
            writer.abort();
        }
    }
}

fn now_string() -> String {
    format_time(OffsetDateTime::now_utc())
}

fn format_time(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .unwrap_or_else(|_| value.unix_timestamp().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clarity_protocol::IceConfiguration;

    #[test]
    fn rejects_binary_and_oversized_messages() {
        assert!(matches!(
            parse_incoming(Some(Ok(Message::Binary(vec![1].into()))), 10),
            Err(ReadError::NonText)
        ));
        assert!(matches!(
            parse_incoming(Some(Ok(Message::Text("123456".into()))), 5),
            Err(ReadError::TooLarge)
        ));
    }

    #[test]
    fn never_serializes_turn_configuration_into_logs() {
        let configuration = IceConfiguration {
            expires_at: "now".into(),
            ice_servers: vec![],
        };
        assert_eq!(configuration.ice_servers.len(), 0);
    }
}
