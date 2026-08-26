use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use clarity_core::{
    DEFAULT_APPROVAL_VIEWERS, DEFAULT_PUBLIC_VIEWERS, PresenceRegistry, RoomEvent, RoomRegistry,
    SecretDigestService, SystemClock, TurnCredentialService, secret_as_str,
};
use clarity_protocol::{
    ApiError, CreateRoomRequest, CreateRoomResponse, ErrorCode, PROTOCOL_VERSION, RoomAccessPolicy,
};
use rust_embed::RustEmbed;
use serde_json::json;
use time::Duration;
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::CompressionLayer,
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};
use uuid::Uuid;

use crate::{
    AppConfig, client_ip::client_ip, config::Environment, rate_limit::RateLimitService, ws,
};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub registry: RoomRegistry,
    pub presence: PresenceRegistry,
    pub turn: Arc<TurnCredentialService>,
    pub rate_limits: RateLimitService,
    ready: Arc<AtomicBool>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppState")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl AppState {
    #[must_use]
    pub fn new(config: AppConfig) -> Self {
        let secrets = Arc::new(SecretDigestService::new(
            config.room_token_hmac_key.clone(),
            config.resume_token_hmac_key.clone(),
        ));
        // Room actors report viewer-count/sharing/close changes on this
        // channel; a forwarder task reflects them into friend presence.
        let (room_events, mut room_events_rx) = tokio::sync::mpsc::channel::<RoomEvent>(256);
        let registry =
            RoomRegistry::new(secrets, config.room_actor.clone()).with_events(room_events);
        let presence = PresenceRegistry::new(Arc::new(SystemClock));
        let presence_sink = presence.clone();
        tokio::spawn(async move {
            while let Some(event) = room_events_rx.recv().await {
                let result = match event {
                    RoomEvent::Updated {
                        room_id,
                        approved_viewers,
                        sharing_state,
                    } => {
                        presence_sink
                            .room_updated(room_id, approved_viewers, sharing_state)
                            .await
                    }
                    RoomEvent::Closed { room_id } => presence_sink.room_closed(room_id).await,
                };
                if result.is_err() {
                    break;
                }
            }
        });
        let turn = Arc::new(TurnCredentialService::new(config.turn.clone()));
        Self {
            config: Arc::new(config),
            registry,
            presence,
            turn,
            rate_limits: RateLimitService::per_minute(),
            ready: Arc::new(AtomicBool::new(true)),
        }
    }
}

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
struct FrontendAssets;

pub fn build_router(state: AppState) -> Router {
    let maximum_body = state.config.websocket_max_message_bytes;
    let production = state.config.environment == Environment::Production;
    let mut router = Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/api/v1/rooms", post(create_room))
        .route("/api/v1/ws", get(ws::upgrade))
        .route("/api/v1/presence", get(crate::presence_ws::upgrade))
        .fallback(spa_fallback)
        .with_state(state)
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::new(header::HeaderName::from_static("x-request-id"), MakeRequestUuid))
        .layer(RequestBodyLimitLayer::new(maximum_body))
        .layer(CompressionLayer::new())
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static("camera=(), microphone=(), display-capture=(self)"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'self'; base-uri 'none'; frame-ancestors 'none'; object-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; media-src 'self' blob:; connect-src 'self' ws: wss:; form-action 'self'"),
        ));
    if production {
        router = router.layer(SetResponseHeaderLayer::if_not_present(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        ));
    }
    router
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "alive" })))
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    if state.ready.load(Ordering::Relaxed) {
        (StatusCode::OK, Json(json!({ "status": "ready" })))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "initializing" })),
        )
    }
}

async fn create_room(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: Result<Json<CreateRoomRequest>, JsonRejection>,
) -> Result<impl IntoResponse, AppError> {
    validate_origin(&state.config, &headers)?;
    let client_ip = client_ip(remote, &headers, state.config.trusted_proxy_hops);
    if !state.rate_limits.check(
        "room-create",
        &client_ip.to_string(),
        state.config.room_creation_rate_limit,
    ) {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::RateLimited,
            "Too many room creation attempts.",
        ));
    }
    let Json(request) = request.map_err(|_| {
        AppError::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidMessage,
            "The room request is malformed.",
        )
    })?;
    let access_policy = request.access_policy.unwrap_or(RoomAccessPolicy::Public);
    let allowed_friend_codes = validate_friend_codes(access_policy, request.allowed_friend_codes)?;
    let maximum_viewers = request.maximum_viewers.unwrap_or(match access_policy {
        RoomAccessPolicy::Public | RoomAccessPolicy::FriendsOnly => DEFAULT_PUBLIC_VIEWERS,
        RoomAccessPolicy::ApprovalRequired => DEFAULT_APPROVAL_VIEWERS,
    });
    let ttl = request
        .expires_in_seconds
        .map_or(state.config.default_room_ttl, |seconds| {
            Duration::seconds(i64::from(seconds))
        });
    if ttl <= Duration::ZERO || ttl > state.config.maximum_room_ttl {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidMessage,
            "Room lifetime is outside the permitted range.",
        ));
    }
    let created = state
        .registry
        .create_room(maximum_viewers, ttl, access_policy, allowed_friend_codes)
        .await
        .map_err(AppError::from_domain)?;
    let mut viewer_url = state
        .config
        .public_base_url
        .join(&format!("r/{}", created.room_id))
        .map_err(|_| AppError::internal())?;
    match created.access_policy {
        RoomAccessPolicy::Public => {
            viewer_url.query_pairs_mut().append_pair("access", "public");
        }
        RoomAccessPolicy::FriendsOnly => {
            viewer_url
                .query_pairs_mut()
                .append_pair("access", "friends");
        }
        RoomAccessPolicy::ApprovalRequired => {}
    }
    viewer_url.set_fragment(Some(secret_as_str(&created.viewer_secret)));
    let response = CreateRoomResponse {
        protocol_version: PROTOCOL_VERSION,
        presenter_path: format!("/present/{}", created.room_id),
        presenter_secret: secret_as_str(&created.presenter_secret).to_owned(),
        viewer_url: viewer_url.to_string(),
        room_id: created.room_id,
        expires_at: created
            .expires_at
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|_| AppError::internal())?,
        maximum_viewers: created.maximum_viewers,
        access_policy: created.access_policy,
    };
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(response),
    ))
}

/// The most friend codes a friends-only room may allowlist.
const MAXIMUM_ALLOWED_FRIEND_CODES: usize = 64;

/// Normalizes the allowlist for a friends-only room. Rejects a missing or
/// empty list, any malformed code, and lists past the size bound; policies
/// other than `FriendsOnly` never carry an allowlist.
fn validate_friend_codes(
    access_policy: RoomAccessPolicy,
    allowed_friend_codes: Option<Vec<String>>,
) -> Result<Vec<String>, AppError> {
    if access_policy != RoomAccessPolicy::FriendsOnly {
        return Ok(Vec::new());
    }
    let codes = allowed_friend_codes.unwrap_or_default();
    if codes.is_empty() || codes.len() > MAXIMUM_ALLOWED_FRIEND_CODES {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidMessage,
            "A friends-only room needs between one and sixty-four friend codes.",
        ));
    }
    codes
        .iter()
        .map(|code| {
            clarity_protocol::code::normalize(code).ok_or_else(|| {
                AppError::new(
                    StatusCode::BAD_REQUEST,
                    ErrorCode::InvalidMessage,
                    "An allowed friend code is not a valid code.",
                )
            })
        })
        .collect()
}

pub fn validate_origin(config: &AppConfig, headers: &HeaderMap) -> Result<(), AppError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            AppError::new(
                StatusCode::FORBIDDEN,
                ErrorCode::OriginRejected,
                "A permitted Origin header is required.",
            )
        })?;
    if config.allowed_origins.contains(origin) {
        Ok(())
    } else {
        Err(AppError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::OriginRejected,
            "The request origin is not permitted.",
        ))
    }
}

async fn spa_fallback(State(_state): State<AppState>, uri: Uri, request: Request) -> Response {
    if uri.path().starts_with("/api/") {
        return AppError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::InvalidMessage,
            "API route not found.",
        )
        .into_response();
    }
    if request.method() != axum::http::Method::GET && request.method() != axum::http::Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("assets/") {
        return embedded_response(path, true)
            .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response());
    }
    let valid_spa_path = path.is_empty() || path.starts_with("present/") || path.starts_with("r/");
    if !valid_spa_path {
        return StatusCode::NOT_FOUND.into_response();
    }
    embedded_response("index.html", false).unwrap_or_else(|| {
        AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Internal,
            "Frontend assets are not available in this build.",
        )
        .into_response()
    })
}

fn embedded_response(path: &str, immutable: bool) -> Option<Response> {
    let asset = FrontendAssets::get(path)?;
    let content_type = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if immutable {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type.as_ref())
        .header(header::CACHE_CONTROL, cache)
        .body(Body::from(asset.data.into_owned()))
        .ok()
}

#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    body: ApiError,
}

impl AppError {
    pub fn new(status: StatusCode, code: ErrorCode, message: &str) -> Self {
        Self {
            status,
            body: ApiError {
                protocol_version: PROTOCOL_VERSION,
                code,
                message: message.to_owned(),
                request_id: Some(Uuid::new_v4().to_string()),
            },
        }
    }

    pub fn from_domain(error: clarity_core::DomainError) -> Self {
        let status = match error {
            clarity_core::DomainError::InvalidCapacity => StatusCode::BAD_REQUEST,
            clarity_core::DomainError::RoomNotFound => StatusCode::NOT_FOUND,
            clarity_core::DomainError::RoomFull
            | clarity_core::DomainError::PendingViewerLimitReached => StatusCode::CONFLICT,
            clarity_core::DomainError::AuthenticationFailed
            | clarity_core::DomainError::AuthorizationDenied => StatusCode::FORBIDDEN,
            clarity_core::DomainError::RoomExpired | clarity_core::DomainError::RoomClosed => {
                StatusCode::GONE
            }
            clarity_core::DomainError::MessageTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            _ => StatusCode::BAD_REQUEST,
        };
        Self::new(status, error.code(), &error.to_string())
    }

    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "The server could not complete the request.",
        )
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            Json(self.body),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_errors_are_json_and_never_cacheable() {
        let response = AppError::new(StatusCode::BAD_REQUEST, ErrorCode::InvalidMessage, "bad")
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    }
}
