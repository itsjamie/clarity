#![forbid(unsafe_code)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const PROTOCOL_VERSION: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PeerRole {
    Presenter,
    Viewer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum RoomLifecycle {
    Open,
    Closed,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ViewerState {
    Pending,
    Approved,
    Disconnected,
    Rejected,
    Kicked,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum RoomAccessPolicy {
    #[default]
    Public,
    ApprovalRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ErrorCode {
    UnsupportedProtocolVersion,
    InvalidMessage,
    AuthenticationRequired,
    AuthenticationFailed,
    AuthorizationDenied,
    RoomNotFound,
    RoomFull,
    RoomExpired,
    RoomClosed,
    ViewerNotFound,
    ViewerNotApproved,
    PendingViewerLimitReached,
    InvalidDestination,
    InvalidCapacity,
    MessageTooLarge,
    RateLimited,
    SessionExpired,
    OriginRejected,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProtocolInfo {
    pub protocol_version: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateRoomRequest {
    pub maximum_viewers: Option<u8>,
    pub expires_in_seconds: Option<u32>,
    pub access_policy: Option<RoomAccessPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateRoomResponse {
    pub protocol_version: u16,
    pub room_id: String,
    pub presenter_secret: String,
    pub presenter_path: String,
    pub viewer_url: String,
    pub expires_at: String,
    pub maximum_viewers: u8,
    pub access_policy: RoomAccessPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ApiError {
    pub protocol_version: u16,
    pub code: ErrorCode,
    pub message: String,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct IceServer {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct IceConfiguration {
    pub expires_at: String,
    pub ice_servers: Vec<IceServer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PeerSnapshot {
    pub peer_id: String,
    pub display_name: Option<String>,
    pub role: PeerRole,
    pub viewer_state: Option<ViewerState>,
    pub connected: bool,
    pub joined_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RoomSnapshot {
    pub room_id: String,
    pub lifecycle: RoomLifecycle,
    pub access_policy: RoomAccessPolicy,
    pub maximum_viewers: u8,
    pub expires_at: String,
    pub presenter_connected: bool,
    pub pending_viewers: Vec<PeerSnapshot>,
    pub approved_viewers: Vec<PeerSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
#[ts(export)]
pub enum ClientMessage {
    #[serde(rename = "auth:presenter")]
    AuthPresenter {
        protocol_version: u16,
        request_id: String,
        room_id: String,
        presenter_secret: String,
    },
    #[serde(rename = "auth:viewer")]
    AuthViewer {
        protocol_version: u16,
        request_id: String,
        room_id: String,
        viewer_secret: String,
        display_name: Option<String>,
    },
    #[serde(rename = "session:resume")]
    SessionResume {
        protocol_version: u16,
        request_id: String,
        room_id: String,
        resume_token: String,
    },
    #[serde(rename = "room:close")]
    RoomClose {
        protocol_version: u16,
        request_id: String,
    },
    #[serde(rename = "room:update-capacity")]
    RoomUpdateCapacity {
        protocol_version: u16,
        request_id: String,
        maximum_viewers: u8,
    },
    #[serde(rename = "viewer:update-display-name")]
    ViewerUpdateDisplayName {
        protocol_version: u16,
        request_id: String,
        display_name: Option<String>,
    },
    #[serde(rename = "viewer:approve")]
    ViewerApprove {
        protocol_version: u16,
        request_id: String,
        peer_id: String,
    },
    #[serde(rename = "viewer:reject")]
    ViewerReject {
        protocol_version: u16,
        request_id: String,
        peer_id: String,
    },
    #[serde(rename = "viewer:kick")]
    ViewerKick {
        protocol_version: u16,
        request_id: String,
        peer_id: String,
    },
    #[serde(rename = "peer:leave")]
    PeerLeave {
        protocol_version: u16,
        request_id: String,
    },
    #[serde(rename = "signal:offer")]
    SignalOffer {
        protocol_version: u16,
        request_id: String,
        destination_peer_id: String,
        sdp: String,
        ice_restart: bool,
    },
    #[serde(rename = "signal:answer")]
    SignalAnswer {
        protocol_version: u16,
        request_id: String,
        destination_peer_id: String,
        sdp: String,
    },
    #[serde(rename = "signal:ice-candidate")]
    SignalIceCandidate {
        protocol_version: u16,
        request_id: String,
        destination_peer_id: String,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_m_line_index: Option<u16>,
    },
    #[serde(rename = "signal:ice-restart")]
    SignalIceRestart {
        protocol_version: u16,
        request_id: String,
        destination_peer_id: String,
    },
    #[serde(rename = "ice:refresh")]
    IceRefresh {
        protocol_version: u16,
        request_id: String,
    },
    #[serde(rename = "heartbeat:pong")]
    HeartbeatPong {
        protocol_version: u16,
        nonce: String,
    },
}

impl ClientMessage {
    #[must_use]
    pub const fn protocol_version(&self) -> u16 {
        match self {
            Self::AuthPresenter {
                protocol_version, ..
            }
            | Self::AuthViewer {
                protocol_version, ..
            }
            | Self::SessionResume {
                protocol_version, ..
            }
            | Self::RoomClose {
                protocol_version, ..
            }
            | Self::RoomUpdateCapacity {
                protocol_version, ..
            }
            | Self::ViewerUpdateDisplayName {
                protocol_version, ..
            }
            | Self::ViewerApprove {
                protocol_version, ..
            }
            | Self::ViewerReject {
                protocol_version, ..
            }
            | Self::ViewerKick {
                protocol_version, ..
            }
            | Self::PeerLeave {
                protocol_version, ..
            }
            | Self::SignalOffer {
                protocol_version, ..
            }
            | Self::SignalAnswer {
                protocol_version, ..
            }
            | Self::SignalIceCandidate {
                protocol_version, ..
            }
            | Self::SignalIceRestart {
                protocol_version, ..
            }
            | Self::IceRefresh {
                protocol_version, ..
            }
            | Self::HeartbeatPong {
                protocol_version, ..
            } => *protocol_version,
        }
    }

    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::HeartbeatPong { .. } => None,
            Self::AuthPresenter { request_id, .. }
            | Self::AuthViewer { request_id, .. }
            | Self::SessionResume { request_id, .. }
            | Self::RoomClose { request_id, .. }
            | Self::RoomUpdateCapacity { request_id, .. }
            | Self::ViewerUpdateDisplayName { request_id, .. }
            | Self::ViewerApprove { request_id, .. }
            | Self::ViewerReject { request_id, .. }
            | Self::ViewerKick { request_id, .. }
            | Self::PeerLeave { request_id, .. }
            | Self::SignalOffer { request_id, .. }
            | Self::SignalAnswer { request_id, .. }
            | Self::SignalIceCandidate { request_id, .. }
            | Self::SignalIceRestart { request_id, .. }
            | Self::IceRefresh { request_id, .. } => Some(request_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
#[ts(export)]
pub enum ServerMessage {
    #[serde(rename = "auth:succeeded")]
    AuthSucceeded {
        protocol_version: u16,
        request_id: String,
        server_timestamp: String,
        room_id: String,
        peer_id: String,
        role: PeerRole,
        resume_token: String,
        resume_expires_at: String,
        snapshot: RoomSnapshot,
        ice_configuration: IceConfiguration,
    },
    #[serde(rename = "auth:failed")]
    AuthFailed {
        protocol_version: u16,
        request_id: String,
        server_timestamp: String,
        code: ErrorCode,
        message: String,
    },
    #[serde(rename = "room:snapshot")]
    RoomSnapshot {
        protocol_version: u16,
        server_timestamp: String,
        snapshot: RoomSnapshot,
    },
    #[serde(rename = "room:closed")]
    RoomClosed {
        protocol_version: u16,
        server_timestamp: String,
    },
    #[serde(rename = "room:expired")]
    RoomExpired {
        protocol_version: u16,
        server_timestamp: String,
    },
    #[serde(rename = "room:capacity-updated")]
    RoomCapacityUpdated {
        protocol_version: u16,
        request_id: String,
        server_timestamp: String,
        maximum_viewers: u8,
    },
    #[serde(rename = "viewer:display-name-updated")]
    ViewerDisplayNameUpdated {
        protocol_version: u16,
        request_id: String,
        server_timestamp: String,
        peer_id: String,
        display_name: Option<String>,
    },
    #[serde(rename = "presenter:disconnected")]
    PresenterDisconnected {
        protocol_version: u16,
        server_timestamp: String,
    },
    #[serde(rename = "presenter:resumed")]
    PresenterResumed {
        protocol_version: u16,
        server_timestamp: String,
    },
    #[serde(rename = "viewer:pending")]
    ViewerPending {
        protocol_version: u16,
        server_timestamp: String,
        viewer: PeerSnapshot,
    },
    #[serde(rename = "viewer:approved")]
    ViewerApproved {
        protocol_version: u16,
        request_id: Option<String>,
        server_timestamp: String,
        peer_id: String,
    },
    #[serde(rename = "viewer:rejected")]
    ViewerRejected {
        protocol_version: u16,
        request_id: Option<String>,
        server_timestamp: String,
        peer_id: String,
    },
    #[serde(rename = "viewer:kicked")]
    ViewerKicked {
        protocol_version: u16,
        request_id: Option<String>,
        server_timestamp: String,
        peer_id: String,
    },
    #[serde(rename = "viewer:left")]
    ViewerLeft {
        protocol_version: u16,
        server_timestamp: String,
        peer_id: String,
    },
    #[serde(rename = "viewer:resumed")]
    ViewerResumed {
        protocol_version: u16,
        server_timestamp: String,
        peer_id: String,
    },
    #[serde(rename = "signal:offer")]
    SignalOffer {
        protocol_version: u16,
        request_id: String,
        server_timestamp: String,
        source_peer_id: String,
        sdp: String,
        ice_restart: bool,
    },
    #[serde(rename = "signal:answer")]
    SignalAnswer {
        protocol_version: u16,
        request_id: String,
        server_timestamp: String,
        source_peer_id: String,
        sdp: String,
    },
    #[serde(rename = "signal:ice-candidate")]
    SignalIceCandidate {
        protocol_version: u16,
        request_id: String,
        server_timestamp: String,
        source_peer_id: String,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_m_line_index: Option<u16>,
    },
    #[serde(rename = "signal:ice-restart")]
    SignalIceRestart {
        protocol_version: u16,
        request_id: String,
        server_timestamp: String,
        source_peer_id: String,
    },
    #[serde(rename = "ice:configuration")]
    IceConfiguration {
        protocol_version: u16,
        request_id: String,
        server_timestamp: String,
        configuration: IceConfiguration,
    },
    #[serde(rename = "heartbeat:ping")]
    HeartbeatPing {
        protocol_version: u16,
        server_timestamp: String,
        nonce: String,
    },
    #[serde(rename = "error")]
    Error {
        protocol_version: u16,
        request_id: Option<String>,
        server_timestamp: String,
        code: ErrorCode,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_messages_use_discriminated_camel_case_json() {
        let value = serde_json::to_value(ClientMessage::ViewerApprove {
            protocol_version: PROTOCOL_VERSION,
            request_id: "r1".into(),
            peer_id: "peer".into(),
        })
        .expect("serializes");

        assert_eq!(value["type"], "viewer:approve");
        assert_eq!(value["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(value["peerId"], "peer");
    }

    #[test]
    fn viewer_display_name_updates_are_explicit_protocol_messages() {
        let value = serde_json::to_value(ClientMessage::ViewerUpdateDisplayName {
            protocol_version: PROTOCOL_VERSION,
            request_id: "rename-1".into(),
            display_name: Some("Jamie".into()),
        })
        .expect("serializes");

        assert_eq!(value["type"], "viewer:update-display-name");
        assert_eq!(value["displayName"], "Jamie");
    }
}
