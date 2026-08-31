use clarity_protocol::{ApiError, CreateRoomRequest, CreateRoomResponse, RoomAccessPolicy};
use url::Url;

use crate::url_authority;

#[derive(Debug, Clone)]
pub struct RoomOptions {
    pub maximum_viewers: u8,
    pub expires_in_seconds: u32,
    pub access_policy: RoomAccessPolicy,
    /// Friend codes permitted to join. Required non-empty when
    /// `access_policy` is [`RoomAccessPolicy::FriendsOnly`]; ignored
    /// otherwise.
    pub allowed_friend_codes: Vec<String>,
}

/// Connection endpoints derived from a Clarity server's base URL: the
/// signaling WebSocket and the origin the server allowlists.
pub struct ServerEndpoints {
    pub signaling_url: String,
    pub origin: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RoomError {
    #[error("the server URL must be an http or https address")]
    InvalidServer,
    #[error("the server could not be reached: {0}")]
    Unreachable(String),
    #[error("the server declined to create a room: {0}")]
    Declined(String),
}

pub fn server_endpoints(server: &Url) -> Result<ServerEndpoints, RoomError> {
    let ws_scheme = match server.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => return Err(RoomError::InvalidServer),
    };
    let authority = url_authority(server).ok_or(RoomError::InvalidServer)?;
    Ok(ServerEndpoints {
        signaling_url: format!("{ws_scheme}://{authority}/api/v1/ws"),
        origin: format!("{}://{authority}", server.scheme()),
    })
}

/// Creates an expiring room, authenticating the request with the server's own
/// origin the same way the web client does.
pub async fn create_room(
    server: &Url,
    options: RoomOptions,
) -> Result<CreateRoomResponse, RoomError> {
    let endpoints = server_endpoints(server)?;
    let url = server
        .join("api/v1/rooms")
        .map_err(|_| RoomError::InvalidServer)?;
    let response = reqwest::Client::new()
        .post(url)
        .header("Origin", &endpoints.origin)
        .json(&CreateRoomRequest {
            maximum_viewers: Some(options.maximum_viewers),
            expires_in_seconds: Some(options.expires_in_seconds),
            access_policy: Some(options.access_policy),
            allowed_friend_codes: if options.allowed_friend_codes.is_empty() {
                None
            } else {
                Some(options.allowed_friend_codes)
            },
        })
        .send()
        .await
        .map_err(|error| RoomError::Unreachable(error.to_string()))?;
    if !response.status().is_success() {
        let status = response.status();
        let message = response
            .json::<ApiError>()
            .await
            .map(|error| error.message)
            .unwrap_or_else(|_| format!("HTTP {status}"));
        return Err(RoomError::Declined(message));
    }
    response
        .json::<CreateRoomResponse>()
        .await
        .map_err(|error| RoomError::Unreachable(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_signaling_endpoints_from_the_server_url() {
        let endpoints = server_endpoints(&Url::parse("http://127.0.0.1:3000").unwrap()).unwrap();
        assert_eq!(endpoints.signaling_url, "ws://127.0.0.1:3000/api/v1/ws");
        assert_eq!(endpoints.origin, "http://127.0.0.1:3000");

        let endpoints =
            server_endpoints(&Url::parse("https://share.example.com").unwrap()).unwrap();
        assert_eq!(endpoints.signaling_url, "wss://share.example.com/api/v1/ws");
        assert_eq!(endpoints.origin, "https://share.example.com");

        let endpoints = server_endpoints(&Url::parse("http://[::1]:3000").unwrap()).unwrap();
        assert_eq!(endpoints.signaling_url, "ws://[::1]:3000/api/v1/ws");
        assert_eq!(endpoints.origin, "http://[::1]:3000");

        assert!(matches!(
            server_endpoints(&Url::parse("ftp://example.com").unwrap()),
            Err(RoomError::InvalidServer)
        ));
    }
}
