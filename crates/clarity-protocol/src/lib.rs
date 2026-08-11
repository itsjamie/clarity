#![forbid(unsafe_code)]

pub mod code;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const PROTOCOL_VERSION: u16 = 5;

/// Context tag for identity signatures answering a room's friends-only
/// challenge (`auth:identity`).
pub const IDENTITY_CONTEXT_ROOM_AUTH: &str = "room-auth";

/// Context tag for identity signatures in the presence handshake
/// (`presence:hello`).
pub const IDENTITY_CONTEXT_PRESENCE: &str = "presence";

/// The exact string an identity signs to answer an identity challenge.
///
/// Binding a context tag and the server's `host[:port]` into the signed bytes
/// gives every signature a single purpose on a single server: a hostile
/// server relaying another server's nonce (or replaying a presence signature
/// into room authentication) gets a signature over the wrong payload, which
/// never verifies. `server_host` is the authority of the URL the client
/// dialed, with default ports omitted; the server accepts its public base URL
/// and each allowed origin.
///
/// Mirrored by the web client in
/// `web/src/lib/identity/identity-challenge.ts`.
#[must_use]
pub fn identity_challenge_payload(context: &str, server_host: &str, nonce: &str) -> String {
    format!("clarity-identity:v1:{context}:{server_host}:{nonce}")
}

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SharingState {
    #[default]
    Idle,
    Live,
    Paused,
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
    FriendsOnly,
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
    /// Friend codes permitted to join. Required (and non-empty) when
    /// `access_policy` is `FriendsOnly`; ignored otherwise.
    pub allowed_friend_codes: Option<Vec<String>>,
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
    /// The peer's friend code, present when the peer proved its identity
    /// during authentication (friends-only rooms).
    pub friend_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RoomSnapshot {
    pub room_id: String,
    pub lifecycle: RoomLifecycle,
    pub sharing_state: SharingState,
    pub access_policy: RoomAccessPolicy,
    pub maximum_viewers: u8,
    pub expires_at: String,
    /// Seconds until the room expires, measured on the server clock when the
    /// snapshot was taken, so clients can render remaining time without
    /// trusting their own clock.
    #[ts(type = "number")]
    pub expires_in_seconds: u64,
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
    #[serde(rename = "auth:identity")]
    AuthIdentity {
        protocol_version: u16,
        request_id: String,
        /// Base64 of the 32-byte Ed25519 public key. The server derives the
        /// friend code from it, so the client never asserts its own code.
        public_key: String,
        /// Base64 Ed25519 signature over the UTF-8 bytes of
        /// [`identity_challenge_payload`] with the
        /// [`IDENTITY_CONTEXT_ROOM_AUTH`] context.
        signature: String,
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
    #[serde(rename = "room:update-sharing-state")]
    RoomUpdateSharingState {
        protocol_version: u16,
        request_id: String,
        sharing_state: SharingState,
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
            | Self::AuthIdentity {
                protocol_version, ..
            }
            | Self::SessionResume {
                protocol_version, ..
            }
            | Self::RoomClose {
                protocol_version, ..
            }
            | Self::RoomUpdateSharingState {
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
            | Self::AuthIdentity { request_id, .. }
            | Self::SessionResume { request_id, .. }
            | Self::RoomClose { request_id, .. }
            | Self::RoomUpdateSharingState { request_id, .. }
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
    #[serde(rename = "auth:identity-challenge")]
    AuthIdentityChallenge {
        protocol_version: u16,
        request_id: String,
        server_timestamp: String,
        /// Nonce the viewer must sign with its Ed25519 key (via
        /// `auth:identity`) to prove its friend code for a friends-only room.
        nonce: String,
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
    #[serde(rename = "room:sharing-state-updated")]
    RoomSharingStateUpdated {
        protocol_version: u16,
        request_id: String,
        server_timestamp: String,
        sharing_state: SharingState,
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

/// The JSON payload of the peer-to-peer `chat` data channel. Chat travels
/// directly between peers over WebRTC with the presenter relaying between
/// viewers; the server never sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChatMessage {
    pub sender: String,
    pub text: String,
}

/// A room a friend is currently hosting, surfaced to their online friends so
/// they can join. The `viewer_url` carries the viewer secret in its fragment,
/// exactly as returned by room creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HostedRoom {
    pub room_id: String,
    pub viewer_url: String,
    pub viewer_count: u32,
    pub sharing_state: SharingState,
}

/// One friend's presence, as seen by a mutually-added friend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FriendPresence {
    /// The friend's code (`clr-XXXX-XXXX`).
    pub code: String,
    pub online: bool,
    /// Present when the friend is sharing a room.
    pub hosting: Option<HostedRoom>,
    /// How long ago the friend was last seen, `None` while online. Tracked
    /// in memory only, so it resets when the server restarts.
    #[ts(type = "number | null")]
    pub last_seen_seconds_ago: Option<u64>,
}

/// Messages a client sends on the presence channel. The handshake is:
/// server `Challenge` → client `Hello` (signs the challenge payload) → server
/// `Ready`, after which the client `Subscribe`s to its contacts and
/// `Announce`s its own activity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
#[ts(export)]
pub enum PresenceClientMessage {
    #[serde(rename = "presence:hello")]
    Hello {
        protocol_version: u16,
        /// Base64 of the 32-byte Ed25519 public key. The server derives the
        /// friend code from it, so the client never asserts its own code.
        public_key: String,
        /// Base64 Ed25519 signature over the UTF-8 bytes of
        /// [`identity_challenge_payload`] with the
        /// [`IDENTITY_CONTEXT_PRESENCE`] context.
        signature: String,
    },
    #[serde(rename = "presence:subscribe")]
    Subscribe {
        protocol_version: u16,
        /// The full set of friend codes to watch; replaces any previous set.
        codes: Vec<String>,
    },
    #[serde(rename = "presence:announce")]
    Announce {
        protocol_version: u16,
        /// The room being hosted now, or `None` when not sharing.
        hosting: Option<HostedRoom>,
        /// The room's presenter secret, proving the announcer hosts the
        /// announced room. Required when `hosting` is `Some`; the server
        /// drops announcements it cannot verify. Never forwarded to friends.
        presenter_secret: Option<String>,
    },
}

/// Messages the server sends on the presence channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
#[ts(export)]
pub enum PresenceServerMessage {
    #[serde(rename = "presence:challenge")]
    Challenge {
        protocol_version: u16,
        server_timestamp: String,
        nonce: String,
    },
    #[serde(rename = "presence:ready")]
    Ready {
        protocol_version: u16,
        server_timestamp: String,
        /// The friend code the server derived from the presented public key.
        code: String,
    },
    #[serde(rename = "presence:snapshot")]
    Snapshot {
        protocol_version: u16,
        server_timestamp: String,
        /// Every currently-visible friend (mutually added and online).
        friends: Vec<FriendPresence>,
    },
    #[serde(rename = "presence:update")]
    Update {
        protocol_version: u16,
        server_timestamp: String,
        friend: FriendPresence,
    },
    #[serde(rename = "error")]
    Error {
        protocol_version: u16,
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

    #[test]
    fn identity_authentication_messages_round_trip() {
        let value = serde_json::to_value(ClientMessage::AuthIdentity {
            protocol_version: PROTOCOL_VERSION,
            request_id: "auth-1".into(),
            public_key: "pk".into(),
            signature: "sig".into(),
        })
        .expect("serializes");
        assert_eq!(value["type"], "auth:identity");
        assert_eq!(value["publicKey"], "pk");

        let value = serde_json::to_value(ServerMessage::AuthIdentityChallenge {
            protocol_version: PROTOCOL_VERSION,
            request_id: "auth-1".into(),
            server_timestamp: "now".into(),
            nonce: "nonce".into(),
        })
        .expect("serializes");
        assert_eq!(value["type"], "auth:identity-challenge");
        assert_eq!(value["nonce"], "nonce");
    }

    #[test]
    fn chat_messages_are_camel_case_json() {
        let value = serde_json::to_value(ChatMessage {
            sender: "Jamie".into(),
            text: "hello".into(),
        })
        .expect("serializes");
        assert_eq!(value, serde_json::json!({ "sender": "Jamie", "text": "hello" }));
    }

    #[test]
    fn sharing_state_updates_are_explicit_protocol_messages() {
        let value = serde_json::to_value(ClientMessage::RoomUpdateSharingState {
            protocol_version: PROTOCOL_VERSION,
            request_id: "pause-1".into(),
            sharing_state: SharingState::Paused,
        })
        .expect("serializes");

        assert_eq!(value["type"], "room:update-sharing-state");
        assert_eq!(value["sharingState"], "paused");
    }
}
