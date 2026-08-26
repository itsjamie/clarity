use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use clarity_protocol::{
    ErrorCode, PROTOCOL_VERSION, PeerRole, PeerSnapshot, RoomAccessPolicy, RoomLifecycle,
    RoomSnapshot, ServerMessage, SharingState, ViewerState,
};
use secrecy::SecretString;
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use tokio::sync::{RwLock, mpsc, oneshot};
use tracing::{debug, warn};

use crate::clock::format_time;
use crate::{Clock, SecretDigest, SecretDigestService, SystemClock};

pub const MAXIMUM_VIEWERS_LIMIT: u8 = 10;
pub const DEFAULT_PUBLIC_VIEWERS: u8 = 10;
pub const DEFAULT_APPROVAL_VIEWERS: u8 = 4;

pub type CommandResult<T = ()> = Result<T, DomainError>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("authentication failed")]
    AuthenticationFailed,
    #[error("a proven identity is required to join this room")]
    IdentityRequired,
    #[error("the resumable session has expired")]
    SessionExpired,
    #[error("authorization denied")]
    AuthorizationDenied,
    #[error("room is full")]
    RoomFull,
    #[error("room expired")]
    RoomExpired,
    #[error("room closed")]
    RoomClosed,
    #[error("viewer not found")]
    ViewerNotFound,
    #[error("viewer is not approved")]
    ViewerNotApproved,
    #[error("pending viewer limit reached")]
    PendingViewerLimitReached,
    #[error("invalid destination")]
    InvalidDestination,
    #[error("capacity must be between one and ten")]
    InvalidCapacity,
    #[error("room not found")]
    RoomNotFound,
    #[error("room command channel is unavailable")]
    Unavailable,
    #[error("message exceeds the configured size limit")]
    MessageTooLarge,
}

impl DomainError {
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::AuthenticationFailed => ErrorCode::AuthenticationFailed,
            Self::IdentityRequired => ErrorCode::AuthenticationRequired,
            Self::SessionExpired => ErrorCode::SessionExpired,
            Self::AuthorizationDenied => ErrorCode::AuthorizationDenied,
            Self::RoomFull => ErrorCode::RoomFull,
            Self::RoomExpired => ErrorCode::RoomExpired,
            Self::RoomClosed => ErrorCode::RoomClosed,
            Self::ViewerNotFound => ErrorCode::ViewerNotFound,
            Self::ViewerNotApproved => ErrorCode::ViewerNotApproved,
            Self::PendingViewerLimitReached => ErrorCode::PendingViewerLimitReached,
            Self::InvalidDestination => ErrorCode::InvalidDestination,
            Self::InvalidCapacity => ErrorCode::InvalidCapacity,
            Self::RoomNotFound => ErrorCode::RoomNotFound,
            Self::Unavailable => ErrorCode::Internal,
            Self::MessageTooLarge => ErrorCode::MessageTooLarge,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RoomActorConfig {
    pub maximum_viewers_hard_limit: u8,
    pub maximum_pending_viewers: usize,
    pub pending_viewer_ttl: Duration,
    pub presenter_resume_grace: Duration,
    pub viewer_resume_grace: Duration,
    pub outbound_capacity: usize,
}

impl Default for RoomActorConfig {
    fn default() -> Self {
        Self {
            maximum_viewers_hard_limit: MAXIMUM_VIEWERS_LIMIT,
            maximum_pending_viewers: 16,
            pending_viewer_ttl: Duration::minutes(2),
            presenter_resume_grace: Duration::seconds(60),
            viewer_resume_grace: Duration::seconds(60),
            outbound_capacity: 128,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionHandle {
    pub outbound: mpsc::Sender<ServerMessage>,
    pub connection_id: String,
    pub close: mpsc::UnboundedSender<()>,
}

/// One concrete transport connection for a peer. Peer ids survive reconnects;
/// connection ids do not, so superseded sockets cannot act as their replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionIdentity {
    pub peer_id: String,
    pub connection_id: String,
}

pub struct AuthOutcome {
    pub room_id: String,
    pub peer_id: String,
    pub connection_id: String,
    pub role: PeerRole,
    pub resume_token: SecretString,
    pub resume_expires_at: OffsetDateTime,
    pub snapshot: RoomSnapshot,
    pub resumed: bool,
}

impl std::fmt::Debug for AuthOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthOutcome")
            .field("room_id", &self.room_id)
            .field("peer_id", &self.peer_id)
            .field("role", &self.role)
            .field("resume_token", &"[REDACTED]")
            .field("resume_expires_at", &self.resume_expires_at)
            .field("resumed", &self.resumed)
            .finish_non_exhaustive()
    }
}

pub struct CreateRoomOutcome {
    pub room_id: String,
    pub presenter_secret: SecretString,
    pub viewer_secret: SecretString,
    pub expires_at: OffsetDateTime,
    pub maximum_viewers: u8,
    pub access_policy: RoomAccessPolicy,
}

#[derive(Debug)]
pub enum RoutedSignal {
    Offer {
        sdp: String,
        ice_restart: bool,
    },
    Answer {
        sdp: String,
    },
    IceCandidate {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_m_line_index: Option<u16>,
    },
    IceRestart,
}

#[derive(Debug)]
pub enum RoomCommand {
    AuthenticatePresenter {
        credential: SecretString,
        session: SessionHandle,
        reply: oneshot::Sender<CommandResult<AuthOutcome>>,
    },
    AuthenticateViewer {
        credential: SecretString,
        display_name: Option<String>,
        /// The viewer's friend code, verified by the transport via the
        /// identity challenge. Required for friends-only rooms.
        friend_code: Option<String>,
        session: SessionHandle,
        reply: oneshot::Sender<CommandResult<AuthOutcome>>,
    },
    Resume {
        resume_token: SecretString,
        session: SessionHandle,
        reply: oneshot::Sender<CommandResult<AuthOutcome>>,
    },
    Approve {
        source: ConnectionIdentity,
        target_peer_id: String,
        request_id: String,
        reply: oneshot::Sender<CommandResult>,
    },
    Reject {
        source: ConnectionIdentity,
        target_peer_id: String,
        request_id: String,
        reply: oneshot::Sender<CommandResult>,
    },
    Kick {
        source: ConnectionIdentity,
        target_peer_id: String,
        request_id: String,
        reply: oneshot::Sender<CommandResult>,
    },
    UpdateCapacity {
        source: ConnectionIdentity,
        maximum_viewers: u8,
        request_id: String,
        reply: oneshot::Sender<CommandResult>,
    },
    UpdateSharingState {
        source: ConnectionIdentity,
        sharing_state: SharingState,
        request_id: String,
        reply: oneshot::Sender<CommandResult>,
    },
    UpdateViewerDisplayName {
        source: ConnectionIdentity,
        display_name: Option<String>,
        request_id: String,
        reply: oneshot::Sender<CommandResult>,
    },
    RouteSignal {
        source: ConnectionIdentity,
        destination_peer_id: String,
        request_id: String,
        signal: RoutedSignal,
        reply: oneshot::Sender<CommandResult>,
    },
    Disconnect {
        session: ConnectionIdentity,
    },
    Leave {
        session: ConnectionIdentity,
    },
    Close {
        source: ConnectionIdentity,
        reply: oneshot::Sender<CommandResult>,
    },
    ValidateConnection {
        source: ConnectionIdentity,
        reply: oneshot::Sender<CommandResult>,
    },
    Snapshot {
        reply: oneshot::Sender<RoomSnapshot>,
    },
    /// Checks a presenter credential without creating or touching a session.
    /// Used by the presence channel to prove a hosting announcement comes
    /// from the room's presenter.
    VerifyPresenter {
        credential: SecretString,
        reply: oneshot::Sender<bool>,
    },
    Shutdown,
}

#[derive(Debug)]
struct PeerSession {
    peer_id: String,
    connection_id: String,
    role: PeerRole,
    display_name: Option<String>,
    friend_code: Option<String>,
    viewer_state: Option<ViewerState>,
    joined_at: OffsetDateTime,
    connected: bool,
    outbound: Option<mpsc::Sender<ServerMessage>>,
    close: mpsc::UnboundedSender<()>,
    resume_digest: SecretDigest,
    resume_expires_at: OffsetDateTime,
    disconnected_at: Option<OffsetDateTime>,
}

pub struct RoomState {
    room_id: String,
    lifecycle: RoomLifecycle,
    sharing_state: SharingState,
    access_policy: RoomAccessPolicy,
    allowed_friend_codes: HashSet<String>,
    maximum_viewers: u8,
    maximum_viewers_hard_limit: u8,
    maximum_pending_viewers: usize,
    pending_viewer_ttl: Duration,
    presenter_resume_grace: Duration,
    viewer_resume_grace: Duration,
    created_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    presenter_digest: SecretDigest,
    viewer_digest: SecretDigest,
    presenter: Option<PeerSession>,
    viewers: HashMap<String, PeerSession>,
    secrets: Arc<SecretDigestService>,
}

impl std::fmt::Debug for RoomState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RoomState")
            .field("room_id", &self.room_id)
            .field("lifecycle", &self.lifecycle)
            .field("sharing_state", &self.sharing_state)
            .field("access_policy", &self.access_policy)
            .field("maximum_viewers", &self.maximum_viewers)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field(
                "presenter_connected",
                &self.presenter.as_ref().is_some_and(|peer| peer.connected),
            )
            .field("viewer_count", &self.viewers.len())
            .finish()
    }
}

impl RoomState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        room_id: String,
        maximum_viewers: u8,
        access_policy: RoomAccessPolicy,
        allowed_friend_codes: HashSet<String>,
        created_at: OffsetDateTime,
        expires_at: OffsetDateTime,
        presenter_digest: SecretDigest,
        viewer_digest: SecretDigest,
        secrets: Arc<SecretDigestService>,
        config: &RoomActorConfig,
    ) -> CommandResult<Self> {
        if maximum_viewers == 0 || maximum_viewers > config.maximum_viewers_hard_limit {
            return Err(DomainError::InvalidCapacity);
        }
        Ok(Self {
            room_id,
            lifecycle: RoomLifecycle::Open,
            sharing_state: SharingState::Idle,
            access_policy,
            allowed_friend_codes,
            maximum_viewers,
            maximum_viewers_hard_limit: config.maximum_viewers_hard_limit,
            maximum_pending_viewers: config.maximum_pending_viewers,
            pending_viewer_ttl: config.pending_viewer_ttl,
            presenter_resume_grace: config.presenter_resume_grace,
            viewer_resume_grace: config.viewer_resume_grace,
            created_at,
            expires_at,
            presenter_digest,
            viewer_digest,
            presenter: None,
            viewers: HashMap::new(),
            secrets,
        })
    }

    #[must_use]
    pub fn snapshot(&self, now: OffsetDateTime) -> RoomSnapshot {
        let mut pending_viewers = self
            .viewers
            .values()
            .filter(|peer| peer.viewer_state == Some(ViewerState::Pending))
            .map(PeerSession::snapshot)
            .collect::<Vec<_>>();
        let mut approved_viewers = self
            .viewers
            .values()
            .filter(|peer| {
                matches!(
                    peer.viewer_state,
                    Some(ViewerState::Approved | ViewerState::Disconnected)
                )
            })
            .map(PeerSession::snapshot)
            .collect::<Vec<_>>();
        pending_viewers.sort_by(|left, right| left.joined_at.cmp(&right.joined_at));
        approved_viewers.sort_by(|left, right| left.joined_at.cmp(&right.joined_at));
        RoomSnapshot {
            room_id: self.room_id.clone(),
            lifecycle: self.lifecycle,
            sharing_state: self.sharing_state,
            access_policy: self.access_policy,
            maximum_viewers: self.maximum_viewers,
            expires_at: format_time(self.expires_at),
            expires_in_seconds: u64::try_from((self.expires_at - now).whole_seconds()).unwrap_or(0),
            presenter_connected: self.presenter.as_ref().is_some_and(|peer| peer.connected),
            pending_viewers,
            approved_viewers,
        }
    }

    /// Approved viewers, including ones inside their resume grace window.
    #[must_use]
    pub fn approved_viewer_count(&self) -> u32 {
        let count = self
            .viewers
            .values()
            .filter(|viewer| {
                matches!(
                    viewer.viewer_state,
                    Some(ViewerState::Approved | ViewerState::Disconnected)
                )
            })
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    #[must_use]
    pub const fn sharing_state(&self) -> SharingState {
        self.sharing_state
    }

    #[must_use]
    pub const fn lifecycle(&self) -> RoomLifecycle {
        self.lifecycle
    }

    fn ensure_open(&self, now: OffsetDateTime) -> CommandResult {
        if now >= self.expires_at || self.lifecycle == RoomLifecycle::Expired {
            return Err(DomainError::RoomExpired);
        }
        if self.lifecycle == RoomLifecycle::Closed {
            return Err(DomainError::RoomClosed);
        }
        Ok(())
    }

    /// Whether `credential` is this room's presenter secret. A pure check: no
    /// session is created or touched.
    #[must_use]
    pub fn verify_presenter(&self, credential: &SecretString) -> bool {
        self.secrets.verify(
            &self.presenter_digest,
            &self.secrets.presenter_digest(credential),
        )
    }

    fn authenticate_presenter(
        &mut self,
        credential: &SecretString,
        session: SessionHandle,
        now: OffsetDateTime,
    ) -> CommandResult<AuthOutcome> {
        self.ensure_open(now)?;
        if !self.verify_presenter(credential) {
            return Err(DomainError::AuthenticationFailed);
        }
        if let Some(previous) = self.presenter.as_mut() {
            previous.connected = false;
            previous.outbound = None;
            previous.disconnected_at = Some(now);
        }
        let resume_token = self.secrets.generate_resume_token();
        let resume_expires_at = now + self.presenter_resume_grace;
        let peer_id = self.presenter.as_ref().map_or_else(
            || self.secrets.generate_peer_id(),
            |peer| peer.peer_id.clone(),
        );
        self.presenter = Some(PeerSession {
            peer_id: peer_id.clone(),
            connection_id: session.connection_id.clone(),
            role: PeerRole::Presenter,
            display_name: None,
            friend_code: None,
            viewer_state: None,
            joined_at: self.presenter.as_ref().map_or(now, |peer| peer.joined_at),
            connected: true,
            outbound: Some(session.outbound),
            close: session.close,
            resume_digest: self.secrets.resume_digest(&resume_token),
            resume_expires_at,
            disconnected_at: None,
        });
        Ok(AuthOutcome {
            room_id: self.room_id.clone(),
            peer_id,
            connection_id: session.connection_id,
            role: PeerRole::Presenter,
            resume_token,
            resume_expires_at,
            snapshot: self.snapshot(now),
            resumed: false,
        })
    }

    fn authenticate_viewer(
        &mut self,
        credential: &SecretString,
        display_name: Option<String>,
        friend_code: Option<String>,
        session: SessionHandle,
        now: OffsetDateTime,
    ) -> CommandResult<AuthOutcome> {
        self.ensure_open(now)?;
        if !self
            .secrets
            .verify(&self.viewer_digest, &self.secrets.viewer_digest(credential))
        {
            return Err(DomainError::AuthenticationFailed);
        }
        if self.access_policy == RoomAccessPolicy::FriendsOnly {
            // The transport verified the Ed25519 proof behind this code; the
            // room only decides whether that code is on the allowlist.
            let Some(code) = friend_code.as_deref() else {
                return Err(DomainError::IdentityRequired);
            };
            if !self.allowed_friend_codes.contains(code) {
                return Err(DomainError::AuthenticationFailed);
            }
        }
        // Friends-only admits like Public once the identity is proven.
        let auto_approved = self.access_policy != RoomAccessPolicy::ApprovalRequired;
        if auto_approved {
            if usize::try_from(self.approved_viewer_count()).unwrap_or(usize::MAX)
                >= usize::from(self.maximum_viewers)
            {
                return Err(DomainError::RoomFull);
            }
        } else {
            let pending_count = self
                .viewers
                .values()
                .filter(|viewer| viewer.viewer_state == Some(ViewerState::Pending))
                .count();
            if pending_count >= self.maximum_pending_viewers {
                return Err(DomainError::PendingViewerLimitReached);
            }
        }
        let peer_id = self.secrets.generate_peer_id();
        let resume_token = self.secrets.generate_resume_token();
        let resume_expires_at = now + self.viewer_resume_grace;
        let viewer = PeerSession {
            peer_id: peer_id.clone(),
            connection_id: session.connection_id.clone(),
            role: PeerRole::Viewer,
            display_name: display_name.and_then(|name| sanitize_display_name(&name)),
            friend_code,
            viewer_state: Some(if auto_approved {
                ViewerState::Approved
            } else {
                ViewerState::Pending
            }),
            joined_at: now,
            connected: true,
            outbound: Some(session.outbound),
            close: session.close,
            resume_digest: self.secrets.resume_digest(&resume_token),
            resume_expires_at,
            disconnected_at: None,
        };
        let viewer_snapshot = viewer.snapshot();
        self.viewers.insert(peer_id.clone(), viewer);
        if auto_approved {
            self.send_snapshot_to_presenter(now);
        } else {
            self.send_presenter(ServerMessage::ViewerPending {
                protocol_version: PROTOCOL_VERSION,
                server_timestamp: format_time(now),
                viewer: viewer_snapshot,
            });
        }
        Ok(AuthOutcome {
            room_id: self.room_id.clone(),
            peer_id,
            connection_id: session.connection_id,
            role: PeerRole::Viewer,
            resume_token,
            resume_expires_at,
            snapshot: self.snapshot(now),
            resumed: false,
        })
    }

    fn resume(
        &mut self,
        token: &SecretString,
        session: SessionHandle,
        now: OffsetDateTime,
    ) -> CommandResult<AuthOutcome> {
        self.ensure_open(now)?;
        let digest = self.secrets.resume_digest(token);
        // Match the token first and check the grace window second, so a valid
        // token presented too late yields `SessionExpired` (a signal to fall
        // back to fresh authentication) rather than a generic failure.
        let presenter_resume = if let Some(presenter) = self.presenter.as_mut()
            && self.secrets.verify(&presenter.resume_digest, &digest)
        {
            if presenter.resume_expires_at < now {
                return Err(DomainError::SessionExpired);
            }
            presenter.connected = true;
            presenter.outbound = Some(session.outbound.clone());
            presenter.connection_id.clone_from(&session.connection_id);
            presenter.close = session.close.clone();
            presenter.disconnected_at = None;
            Some((presenter.peer_id.clone(), presenter.resume_expires_at))
        } else {
            None
        };
        if let Some((peer_id, resume_expires_at)) = presenter_resume {
            self.broadcast_viewers(ServerMessage::PresenterResumed {
                protocol_version: PROTOCOL_VERSION,
                server_timestamp: format_time(now),
            });
            return Ok(AuthOutcome {
                room_id: self.room_id.clone(),
                peer_id,
                connection_id: session.connection_id.clone(),
                role: PeerRole::Presenter,
                resume_token: token.clone(),
                resume_expires_at,
                snapshot: self.snapshot(now),
                resumed: true,
            });
        }
        let viewer_id = self
            .viewers
            .iter()
            .find(|(_, viewer)| self.secrets.verify(&viewer.resume_digest, &digest))
            .map(|(peer_id, _)| peer_id.clone());
        if let Some(viewer_id) = viewer_id
            && let Some(viewer) = self.viewers.get_mut(&viewer_id)
        {
            if viewer.resume_expires_at < now {
                return Err(DomainError::SessionExpired);
            }
            viewer.connected = true;
            viewer.outbound = Some(session.outbound);
            viewer.connection_id.clone_from(&session.connection_id);
            viewer.close = session.close;
            viewer.disconnected_at = None;
            if viewer.viewer_state == Some(ViewerState::Disconnected) {
                viewer.viewer_state = Some(ViewerState::Approved);
            }
            let peer_id = viewer.peer_id.clone();
            let resume_expires_at = viewer.resume_expires_at;
            self.send_presenter(ServerMessage::ViewerResumed {
                protocol_version: PROTOCOL_VERSION,
                server_timestamp: format_time(now),
                peer_id: peer_id.clone(),
            });
            return Ok(AuthOutcome {
                room_id: self.room_id.clone(),
                peer_id,
                connection_id: session.connection_id,
                role: PeerRole::Viewer,
                resume_token: token.clone(),
                resume_expires_at,
                snapshot: self.snapshot(now),
                resumed: true,
            });
        }
        Err(DomainError::AuthenticationFailed)
    }

    fn ensure_presenter(&self, source_peer_id: &str) -> CommandResult {
        if self
            .presenter
            .as_ref()
            .is_some_and(|peer| peer.peer_id == source_peer_id)
        {
            Ok(())
        } else {
            Err(DomainError::AuthorizationDenied)
        }
    }

    fn ensure_connection(&self, source: &ConnectionIdentity) -> CommandResult {
        let active = self
            .presenter
            .as_ref()
            .filter(|peer| peer.peer_id == source.peer_id)
            .or_else(|| self.viewers.get(&source.peer_id))
            .is_some_and(|peer| {
                peer.connected && peer.connection_id == source.connection_id
            });
        if active {
            Ok(())
        } else {
            Err(DomainError::AuthorizationDenied)
        }
    }

    fn approve(
        &mut self,
        source: &str,
        target: &str,
        request_id: String,
        now: OffsetDateTime,
    ) -> CommandResult {
        self.ensure_open(now)?;
        self.ensure_presenter(source)?;
        if usize::try_from(self.approved_viewer_count()).unwrap_or(usize::MAX)
            >= usize::from(self.maximum_viewers)
        {
            return Err(DomainError::RoomFull);
        }
        let viewer = self
            .viewers
            .get_mut(target)
            .ok_or(DomainError::ViewerNotFound)?;
        if viewer.viewer_state != Some(ViewerState::Pending) {
            return Err(DomainError::AuthorizationDenied);
        }
        viewer.viewer_state = Some(ViewerState::Approved);
        try_send(
            viewer,
            ServerMessage::ViewerApproved {
                protocol_version: PROTOCOL_VERSION,
                request_id: Some(request_id),
                server_timestamp: format_time(now),
                peer_id: target.to_owned(),
            },
        );
        self.send_snapshot_to_presenter(now);
        Ok(())
    }

    fn reject(
        &mut self,
        source: &str,
        target: &str,
        request_id: String,
        now: OffsetDateTime,
    ) -> CommandResult {
        self.ensure_presenter(source)?;
        let mut viewer = self
            .viewers
            .remove(target)
            .ok_or(DomainError::ViewerNotFound)?;
        if viewer.viewer_state != Some(ViewerState::Pending) {
            self.viewers.insert(target.to_owned(), viewer);
            return Err(DomainError::AuthorizationDenied);
        }
        viewer.viewer_state = Some(ViewerState::Rejected);
        try_send(
            &mut viewer,
            ServerMessage::ViewerRejected {
                protocol_version: PROTOCOL_VERSION,
                request_id: Some(request_id),
                server_timestamp: format_time(now),
                peer_id: target.to_owned(),
            },
        );
        self.send_snapshot_to_presenter(now);
        Ok(())
    }

    fn kick(
        &mut self,
        source: &str,
        target: &str,
        request_id: String,
        now: OffsetDateTime,
    ) -> CommandResult {
        self.ensure_presenter(source)?;
        let mut viewer = self
            .viewers
            .remove(target)
            .ok_or(DomainError::ViewerNotFound)?;
        if !matches!(
            viewer.viewer_state,
            Some(ViewerState::Approved | ViewerState::Disconnected)
        ) {
            self.viewers.insert(target.to_owned(), viewer);
            return Err(DomainError::ViewerNotApproved);
        }
        viewer.viewer_state = Some(ViewerState::Kicked);
        try_send(
            &mut viewer,
            ServerMessage::ViewerKicked {
                protocol_version: PROTOCOL_VERSION,
                request_id: Some(request_id),
                server_timestamp: format_time(now),
                peer_id: target.to_owned(),
            },
        );
        self.send_snapshot_to_presenter(now);
        Ok(())
    }

    fn update_capacity(
        &mut self,
        source: &str,
        maximum_viewers: u8,
        request_id: String,
        now: OffsetDateTime,
    ) -> CommandResult {
        self.ensure_presenter(source)?;
        if maximum_viewers == 0 || maximum_viewers > self.maximum_viewers_hard_limit {
            return Err(DomainError::InvalidCapacity);
        }
        self.maximum_viewers = maximum_viewers;
        self.send_presenter(ServerMessage::RoomCapacityUpdated {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            server_timestamp: format_time(now),
            maximum_viewers,
        });
        self.send_snapshot_to_presenter(now);
        Ok(())
    }

    fn update_sharing_state(
        &mut self,
        source: &str,
        sharing_state: SharingState,
        request_id: String,
        now: OffsetDateTime,
    ) -> CommandResult {
        self.ensure_open(now)?;
        self.ensure_presenter(source)?;
        self.sharing_state = sharing_state;
        let message = ServerMessage::RoomSharingStateUpdated {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            server_timestamp: format_time(now),
            sharing_state,
        };
        self.send_presenter(message.clone());
        self.broadcast_all_viewers(message);
        Ok(())
    }

    fn update_viewer_display_name(
        &mut self,
        source: &str,
        display_name: Option<String>,
        request_id: String,
        now: OffsetDateTime,
    ) -> CommandResult {
        self.ensure_open(now)?;
        let display_name = display_name.and_then(|name| sanitize_display_name(&name));
        let peer_id = {
            let viewer = self
                .viewers
                .get_mut(source)
                .ok_or(DomainError::AuthorizationDenied)?;
            viewer.display_name = display_name.clone();
            let peer_id = viewer.peer_id.clone();
            try_send(
                viewer,
                ServerMessage::ViewerDisplayNameUpdated {
                    protocol_version: PROTOCOL_VERSION,
                    request_id,
                    server_timestamp: format_time(now),
                    peer_id: peer_id.clone(),
                    display_name,
                },
            );
            peer_id
        };
        debug!(peer_id = %peer_id, "viewer display name updated");
        self.send_snapshot_to_presenter(now);
        Ok(())
    }

    fn route_signal(
        &mut self,
        source: &str,
        destination: &str,
        request_id: String,
        signal: RoutedSignal,
        now: OffsetDateTime,
    ) -> CommandResult {
        self.ensure_open(now)?;
        let source_role = self
            .peer_role(source)
            .ok_or(DomainError::AuthorizationDenied)?;
        let destination_role = self
            .peer_role(destination)
            .ok_or(DomainError::InvalidDestination)?;
        SignalingAuthorizationService::authorize(source_role, destination_role, &signal)?;
        if source_role == PeerRole::Viewer && !self.viewer_is_approved(source) {
            return Err(DomainError::ViewerNotApproved);
        }
        if destination_role == PeerRole::Viewer && !self.viewer_is_approved(destination) {
            return Err(DomainError::ViewerNotApproved);
        }
        let message = match signal {
            RoutedSignal::Offer { sdp, ice_restart } => ServerMessage::SignalOffer {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                server_timestamp: format_time(now),
                source_peer_id: source.to_owned(),
                sdp,
                ice_restart,
            },
            RoutedSignal::Answer { sdp } => ServerMessage::SignalAnswer {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                server_timestamp: format_time(now),
                source_peer_id: source.to_owned(),
                sdp,
            },
            RoutedSignal::IceCandidate {
                candidate,
                sdp_mid,
                sdp_m_line_index,
            } => ServerMessage::SignalIceCandidate {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                server_timestamp: format_time(now),
                source_peer_id: source.to_owned(),
                candidate,
                sdp_mid,
                sdp_m_line_index,
            },
            RoutedSignal::IceRestart => ServerMessage::SignalIceRestart {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                server_timestamp: format_time(now),
                source_peer_id: source.to_owned(),
            },
        };
        self.send_to_peer(destination, message)
    }

    fn peer_role(&self, peer_id: &str) -> Option<PeerRole> {
        if self
            .presenter
            .as_ref()
            .is_some_and(|presenter| presenter.peer_id == peer_id)
        {
            Some(PeerRole::Presenter)
        } else {
            self.viewers.get(peer_id).map(|viewer| viewer.role)
        }
    }

    fn viewer_is_approved(&self, peer_id: &str) -> bool {
        self.viewers.get(peer_id).is_some_and(|viewer| {
            matches!(
                viewer.viewer_state,
                Some(ViewerState::Approved | ViewerState::Disconnected)
            )
        })
    }

    fn send_to_peer(&mut self, peer_id: &str, message: ServerMessage) -> CommandResult {
        if let Some(presenter) = self
            .presenter
            .as_mut()
            .filter(|peer| peer.peer_id == peer_id)
        {
            return if try_send(presenter, message) {
                Ok(())
            } else {
                Err(DomainError::Unavailable)
            };
        }
        let viewer = self
            .viewers
            .get_mut(peer_id)
            .ok_or(DomainError::InvalidDestination)?;
        if try_send(viewer, message) {
            Ok(())
        } else {
            Err(DomainError::Unavailable)
        }
    }

    fn send_presenter(&mut self, message: ServerMessage) {
        if let Some(presenter) = self.presenter.as_mut() {
            try_send(presenter, message);
        }
    }

    fn broadcast_viewers(&mut self, message: ServerMessage) {
        for viewer in self.viewers.values_mut() {
            if viewer.viewer_state == Some(ViewerState::Approved) {
                try_send(viewer, message.clone());
            }
        }
    }

    fn broadcast_all_viewers(&mut self, message: ServerMessage) {
        for viewer in self.viewers.values_mut() {
            if viewer.connected {
                try_send(viewer, message.clone());
            }
        }
    }

    fn send_snapshot_to_presenter(&mut self, now: OffsetDateTime) {
        let snapshot = self.snapshot(now);
        self.send_presenter(ServerMessage::RoomSnapshot {
            protocol_version: PROTOCOL_VERSION,
            server_timestamp: format_time(now),
            snapshot,
        });
    }

    fn disconnect(&mut self, session: &ConnectionIdentity, now: OffsetDateTime) {
        if let Some(presenter) = self
            .presenter
            .as_mut()
            .filter(|peer| {
                peer.peer_id == session.peer_id && peer.connection_id == session.connection_id
            })
        {
            presenter.connected = false;
            presenter.outbound = None;
            presenter.disconnected_at = Some(now);
            presenter.resume_expires_at = now + self.presenter_resume_grace;
            self.broadcast_viewers(ServerMessage::PresenterDisconnected {
                protocol_version: PROTOCOL_VERSION,
                server_timestamp: format_time(now),
            });
            return;
        }
        if let Some(viewer) = self
            .viewers
            .get_mut(&session.peer_id)
            .filter(|peer| peer.connection_id == session.connection_id)
        {
            viewer.connected = false;
            viewer.outbound = None;
            viewer.disconnected_at = Some(now);
            viewer.resume_expires_at = now + self.viewer_resume_grace;
            if viewer.viewer_state == Some(ViewerState::Approved) {
                viewer.viewer_state = Some(ViewerState::Disconnected);
            }
            self.send_snapshot_to_presenter(now);
        }
    }

    fn leave(&mut self, session: &ConnectionIdentity, now: OffsetDateTime) {
        if self.ensure_connection(session).is_err() {
            return;
        }
        let peer_id = &session.peer_id;
        if self
            .presenter
            .as_ref()
            .is_some_and(|peer| peer.peer_id == *peer_id)
        {
            self.close(now, false);
        } else if self.viewers.remove(peer_id).is_some() {
            self.send_presenter(ServerMessage::ViewerLeft {
                protocol_version: PROTOCOL_VERSION,
                server_timestamp: format_time(now),
                peer_id: peer_id.to_owned(),
            });
            self.send_snapshot_to_presenter(now);
        }
    }

    fn expire_stale(&mut self, now: OffsetDateTime) {
        if now >= self.expires_at {
            self.lifecycle = RoomLifecycle::Expired;
            self.close(now, true);
            return;
        }
        if self.presenter.as_ref().is_some_and(|presenter| {
            !presenter.connected
                && presenter
                    .disconnected_at
                    .is_some_and(|at| now - at >= self.presenter_resume_grace)
        }) {
            self.close(now, false);
            return;
        }
        self.viewers.retain(|_, viewer| {
            let pending_expired = viewer.viewer_state == Some(ViewerState::Pending)
                && now - viewer.joined_at >= self.pending_viewer_ttl;
            // Keep a disconnected viewer around for one grace window past its
            // resume expiry, so a late resume gets a precise `SessionExpired`
            // instead of a generic authentication failure.
            let disconnected_expired = !viewer.connected
                && viewer
                    .disconnected_at
                    .is_some_and(|at| now - at >= self.viewer_resume_grace * 2);
            !pending_expired && !disconnected_expired
        });
    }

    fn close(&mut self, now: OffsetDateTime, expired: bool) {
        self.lifecycle = if expired {
            RoomLifecycle::Expired
        } else {
            RoomLifecycle::Closed
        };
        let message = if expired {
            ServerMessage::RoomExpired {
                protocol_version: PROTOCOL_VERSION,
                server_timestamp: format_time(now),
            }
        } else {
            ServerMessage::RoomClosed {
                protocol_version: PROTOCOL_VERSION,
                server_timestamp: format_time(now),
            }
        };
        if let Some(presenter) = self.presenter.as_mut() {
            try_send(presenter, message.clone());
        }
        for viewer in self.viewers.values_mut() {
            try_send(viewer, message.clone());
        }
    }
}

impl PeerSession {
    fn snapshot(&self) -> PeerSnapshot {
        PeerSnapshot {
            peer_id: self.peer_id.clone(),
            display_name: self.display_name.clone(),
            role: self.role,
            viewer_state: self.viewer_state,
            connected: self.connected,
            joined_at: format_time(self.joined_at),
            friend_code: self.friend_code.clone(),
        }
    }
}

fn try_send(peer: &mut PeerSession, message: ServerMessage) -> bool {
    let Some(outbound) = peer.outbound.as_ref() else {
        return false;
    };
    match outbound.try_send(message) {
        Ok(()) => true,
        Err(error) => {
            warn!(peer_id = %peer.peer_id, error = %error, "peer outbound queue unavailable");
            let _ = peer.close.send(());
            peer.connected = false;
            peer.outbound = None;
            false
        }
    }
}

pub struct SignalingAuthorizationService;

impl SignalingAuthorizationService {
    pub fn authorize(
        source: PeerRole,
        destination: PeerRole,
        signal: &RoutedSignal,
    ) -> CommandResult {
        if source == destination {
            return Err(DomainError::AuthorizationDenied);
        }
        match (source, signal) {
            (
                PeerRole::Presenter,
                RoutedSignal::Offer { .. }
                | RoutedSignal::IceCandidate { .. }
                | RoutedSignal::IceRestart,
            ) => Ok(()),
            (
                PeerRole::Viewer,
                RoutedSignal::Answer { .. }
                | RoutedSignal::IceCandidate { .. }
                | RoutedSignal::IceRestart,
            ) => Ok(()),
            _ => Err(DomainError::AuthorizationDenied),
        }
    }
}

/// A change of observable room state, emitted by the room actor for listeners
/// outside the room (such as friend presence). The registry stays unaware of
/// who listens; the server wires the receiving end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomEvent {
    /// The approved viewer count or sharing state changed.
    Updated {
        room_id: String,
        approved_viewers: u32,
        sharing_state: SharingState,
    },
    /// The room closed or expired.
    Closed { room_id: String },
}

type RegistryMap = Arc<RwLock<HashMap<String, mpsc::Sender<RoomCommand>>>>;

#[derive(Clone)]
pub struct RoomRegistry {
    rooms: RegistryMap,
    secrets: Arc<SecretDigestService>,
    clock: Arc<dyn Clock>,
    config: RoomActorConfig,
    events: Option<mpsc::Sender<RoomEvent>>,
}

impl std::fmt::Debug for RoomRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RoomRegistry")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl RoomRegistry {
    #[must_use]
    pub fn new(secrets: Arc<SecretDigestService>, config: RoomActorConfig) -> Self {
        Self::with_clock(secrets, config, Arc::new(SystemClock))
    }

    #[must_use]
    pub fn with_clock(
        secrets: Arc<SecretDigestService>,
        config: RoomActorConfig,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            rooms: Arc::new(RwLock::new(HashMap::new())),
            secrets,
            clock,
            config,
            events: None,
        }
    }

    /// Emits [`RoomEvent`]s from every room actor onto `events`. A slow live
    /// receiver applies backpressure so presence cannot silently diverge from
    /// authoritative room state; a dropped receiver is skipped immediately.
    #[must_use]
    pub fn with_events(mut self, events: mpsc::Sender<RoomEvent>) -> Self {
        self.events = Some(events);
        self
    }

    pub async fn create_room(
        &self,
        maximum_viewers: u8,
        ttl: Duration,
        access_policy: RoomAccessPolicy,
        allowed_friend_codes: Vec<String>,
    ) -> CommandResult<CreateRoomOutcome> {
        if maximum_viewers == 0 || maximum_viewers > self.config.maximum_viewers_hard_limit {
            return Err(DomainError::InvalidCapacity);
        }
        let generated = self.secrets.generate_room_secrets();
        let presenter_digest = self.secrets.presenter_digest(&generated.presenter_secret);
        let viewer_digest = self.secrets.viewer_digest(&generated.viewer_secret);
        let now = self.clock.now();
        let expires_at = now + ttl;
        let state = RoomState::new(
            generated.room_id.clone(),
            maximum_viewers,
            access_policy,
            allowed_friend_codes.into_iter().collect(),
            now,
            expires_at,
            presenter_digest,
            viewer_digest,
            Arc::clone(&self.secrets),
            &self.config,
        )?;
        let (sender, receiver) = mpsc::channel(256);
        self.rooms
            .write()
            .await
            .insert(generated.room_id.clone(), sender);
        let rooms = Arc::clone(&self.rooms);
        let room_id = generated.room_id.clone();
        let clock = Arc::clone(&self.clock);
        let events = self.events.clone();
        tokio::spawn(async move {
            run_room_actor(state, receiver, clock, events).await;
            rooms.write().await.remove(&room_id);
            debug!(room_id = %room_id, "room actor removed from registry");
        });
        Ok(CreateRoomOutcome {
            room_id: generated.room_id,
            presenter_secret: generated.presenter_secret,
            viewer_secret: generated.viewer_secret,
            expires_at,
            maximum_viewers,
            access_policy,
        })
    }

    pub async fn sender(&self, room_id: &str) -> CommandResult<mpsc::Sender<RoomCommand>> {
        self.rooms
            .read()
            .await
            .get(room_id)
            .cloned()
            .ok_or(DomainError::RoomNotFound)
    }

    pub async fn dispatch(&self, room_id: &str, command: RoomCommand) -> CommandResult {
        let sender = self.sender(room_id).await?;
        sender
            .send(command)
            .await
            .map_err(|_| DomainError::Unavailable)
    }

    pub async fn len(&self) -> usize {
        self.rooms.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.rooms.read().await.is_empty()
    }

    pub async fn shutdown(&self) {
        let senders = self
            .rooms
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for sender in senders {
            let _ = sender.send(RoomCommand::Shutdown).await;
        }
    }
}

async fn run_room_actor(
    mut room: RoomState,
    mut commands: mpsc::Receiver<RoomCommand>,
    clock: Arc<dyn Clock>,
    events: Option<mpsc::Sender<RoomEvent>>,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut announced = (room.approved_viewer_count(), room.sharing_state());
    loop {
        tokio::select! {
            _ = interval.tick() => {
                room.expire_stale(clock.now());
                if room.lifecycle != RoomLifecycle::Open { break; }
                emit_room_update(&room, &events, &mut announced).await;
            }
            command = commands.recv() => {
                let Some(command) = command else { break; };
                let now = clock.now();
                match command {
                    RoomCommand::AuthenticatePresenter { credential, session, reply } => {
                        let _ = reply.send(room.authenticate_presenter(&credential, session, now));
                    }
                    RoomCommand::AuthenticateViewer { credential, display_name, friend_code, session, reply } => {
                        let _ = reply.send(room.authenticate_viewer(&credential, display_name, friend_code, session, now));
                    }
                    RoomCommand::Resume { resume_token, session, reply } => {
                        let _ = reply.send(room.resume(&resume_token, session, now));
                    }
                    RoomCommand::Approve { source, target_peer_id, request_id, reply } => {
                        let result = room.ensure_connection(&source).and_then(|()| room.approve(&source.peer_id, &target_peer_id, request_id, now));
                        let _ = reply.send(result);
                    }
                    RoomCommand::Reject { source, target_peer_id, request_id, reply } => {
                        let result = room.ensure_connection(&source).and_then(|()| room.reject(&source.peer_id, &target_peer_id, request_id, now));
                        let _ = reply.send(result);
                    }
                    RoomCommand::Kick { source, target_peer_id, request_id, reply } => {
                        let result = room.ensure_connection(&source).and_then(|()| room.kick(&source.peer_id, &target_peer_id, request_id, now));
                        let _ = reply.send(result);
                    }
                    RoomCommand::UpdateCapacity { source, maximum_viewers, request_id, reply } => {
                        let result = room.ensure_connection(&source).and_then(|()| room.update_capacity(&source.peer_id, maximum_viewers, request_id, now));
                        let _ = reply.send(result);
                    }
                    RoomCommand::UpdateSharingState { source, sharing_state, request_id, reply } => {
                        let result = room.ensure_connection(&source).and_then(|()| room.update_sharing_state(&source.peer_id, sharing_state, request_id, now));
                        let _ = reply.send(result);
                    }
                    RoomCommand::UpdateViewerDisplayName { source, display_name, request_id, reply } => {
                        let result = room.ensure_connection(&source).and_then(|()| room.update_viewer_display_name(&source.peer_id, display_name, request_id, now));
                        let _ = reply.send(result);
                    }
                    RoomCommand::RouteSignal { source, destination_peer_id, request_id, signal, reply } => {
                        let result = room.ensure_connection(&source).and_then(|()| room.route_signal(&source.peer_id, &destination_peer_id, request_id, signal, now));
                        let _ = reply.send(result);
                    }
                    RoomCommand::Disconnect { session } => room.disconnect(&session, now),
                    RoomCommand::Leave { session } => room.leave(&session, now),
                    RoomCommand::Close { source, reply } => {
                        let result = room.ensure_connection(&source).and_then(|()| room.ensure_presenter(&source.peer_id)).map(|()| room.close(now, false));
                        let _ = reply.send(result);
                    }
                    RoomCommand::ValidateConnection { source, reply } => {
                        let _ = reply.send(room.ensure_connection(&source));
                    }
                    RoomCommand::Snapshot { reply } => { let _ = reply.send(room.snapshot(now)); }
                    RoomCommand::VerifyPresenter { credential, reply } => {
                        let _ = reply.send(room.verify_presenter(&credential));
                    }
                    RoomCommand::Shutdown => room.close(now, false),
                }
                if room.lifecycle != RoomLifecycle::Open { break; }
                emit_room_update(&room, &events, &mut announced).await;
            }
        }
    }
    if let Some(events) = events {
        let _ = events.send(RoomEvent::Closed {
            room_id: room.room_id.clone(),
        }).await;
    }
}

/// Sends an [`RoomEvent::Updated`] when the observable room state moved past
/// what was last announced.
async fn emit_room_update(
    room: &RoomState,
    events: &Option<mpsc::Sender<RoomEvent>>,
    announced: &mut (u32, SharingState),
) {
    let Some(events) = events else { return };
    let current = (room.approved_viewer_count(), room.sharing_state());
    if current == *announced {
        return;
    }
    if events.send(RoomEvent::Updated {
        room_id: room.room_id.clone(),
        approved_viewers: current.0,
        sharing_state: current.1,
    }).await.is_ok() {
        *announced = current;
    }
}

#[must_use]
pub fn sanitize_display_name(value: &str) -> Option<String> {
    let normalized = value
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let truncated = normalized.chars().take(48).collect::<String>();
    (!truncated.is_empty()).then_some(truncated)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::{ManualClock, secret_as_str};

    fn fixture() -> (
        RoomState,
        Arc<SecretDigestService>,
        SecretString,
        SecretString,
        OffsetDateTime,
    ) {
        fixture_with_policy(RoomAccessPolicy::ApprovalRequired)
    }

    fn fixture_with_policy(
        access_policy: RoomAccessPolicy,
    ) -> (
        RoomState,
        Arc<SecretDigestService>,
        SecretString,
        SecretString,
        OffsetDateTime,
    ) {
        let secrets = Arc::new(SecretDigestService::new(
            SecretString::from("room-key-with-at-least-32-bytes-long".to_owned()),
            SecretString::from("resume-key-with-at-least-32-bytes".to_owned()),
        ));
        let generated = secrets.generate_room_secrets();
        let presenter = generated.presenter_secret;
        let viewer = generated.viewer_secret;
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("time");
        let room = RoomState::new(
            generated.room_id,
            4,
            access_policy,
            HashSet::new(),
            now,
            now + Duration::hours(2),
            secrets.presenter_digest(&presenter),
            secrets.viewer_digest(&viewer),
            Arc::clone(&secrets),
            &RoomActorConfig::default(),
        )
        .expect("room");
        (room, secrets, presenter, viewer, now)
    }

    fn session() -> (SessionHandle, mpsc::Receiver<ServerMessage>) {
        static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);
        let (outbound, receiver) = mpsc::channel(8);
        let (close, _) = mpsc::unbounded_channel();
        (
            SessionHandle {
                outbound,
                connection_id: format!(
                    "test-{}",
                    NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed)
                ),
                close,
            },
            receiver,
        )
    }

    #[test]
    fn superseded_presenter_connection_cannot_disconnect_or_act_for_replacement() {
        let (mut room, _, presenter_secret, _, now) = fixture();
        let first = room
            .authenticate_presenter(&presenter_secret, session().0, now)
            .expect("first authentication");
        let second = room
            .authenticate_presenter(&presenter_secret, session().0, now)
            .expect("replacement authentication");
        let first = ConnectionIdentity {
            peer_id: first.peer_id,
            connection_id: first.connection_id,
        };
        let second = ConnectionIdentity {
            peer_id: second.peer_id,
            connection_id: second.connection_id,
        };

        room.disconnect(&first, now);

        assert!(room.snapshot(now).presenter_connected);
        assert_eq!(
            room.ensure_connection(&first).expect_err("superseded connection"),
            DomainError::AuthorizationDenied
        );
        room.ensure_connection(&second).expect("replacement connection");
    }

    #[test]
    fn a_full_outbound_queue_requests_transport_shutdown() {
        let (mut room, _, presenter_secret, viewer_secret, now) =
            fixture_with_policy(RoomAccessPolicy::Public);
        let (outbound, _receiver) = mpsc::channel(1);
        let (close, mut close_requests) = mpsc::unbounded_channel();
        room.authenticate_presenter(
            &presenter_secret,
            SessionHandle {
                outbound,
                connection_id: "full-queue-test".to_owned(),
                close,
            },
            now,
        )
        .expect("presenter");

        room.authenticate_viewer(&viewer_secret, None, None, session().0, now)
            .expect("first viewer fills the presenter queue");
        room.authenticate_viewer(&viewer_secret, None, None, session().0, now)
            .expect("second viewer detects the full presenter queue");

        assert_eq!(close_requests.try_recv(), Ok(()));
        assert!(!room.snapshot(now).presenter_connected);
    }

    #[test]
    fn friends_only_rooms_admit_allowlisted_codes_and_reject_others() {
        let (mut room, _, _, viewer_secret, now) = fixture_with_policy(RoomAccessPolicy::Public);
        room.access_policy = RoomAccessPolicy::FriendsOnly;
        room.allowed_friend_codes = HashSet::from(["clr-AAAA-AAAA".to_owned()]);

        // No proven identity: the transport must run the identity challenge.
        assert_eq!(
            room.authenticate_viewer(&viewer_secret, None, None, session().0, now)
                .expect_err("identity required"),
            DomainError::IdentityRequired
        );
        // A code off the allowlist is refused outright.
        assert_eq!(
            room.authenticate_viewer(
                &viewer_secret,
                None,
                Some("clr-BBBB-BBBB".into()),
                session().0,
                now,
            )
            .expect_err("not allowlisted"),
            DomainError::AuthenticationFailed
        );
        // An allowlisted code joins approved immediately, like Public.
        let admitted = room
            .authenticate_viewer(
                &viewer_secret,
                Some("Friend".into()),
                Some("clr-AAAA-AAAA".into()),
                session().0,
                now,
            )
            .expect("allowlisted friend");
        let snapshot = room.snapshot(now);
        assert!(snapshot.pending_viewers.is_empty());
        assert_eq!(
            snapshot
                .approved_viewers
                .iter()
                .find(|peer| peer.peer_id == admitted.peer_id)
                .and_then(|peer| peer.friend_code.as_deref()),
            Some("clr-AAAA-AAAA")
        );
    }

    #[test]
    fn late_resume_with_a_valid_token_reports_session_expired() {
        let (mut room, _, presenter_secret, viewer_secret, now) =
            fixture_with_policy(RoomAccessPolicy::Public);
        room.authenticate_presenter(&presenter_secret, session().0, now)
            .expect("presenter");
        let viewer = room
            .authenticate_viewer(&viewer_secret, None, None, session().0, now)
            .expect("viewer");
        room.disconnect(
            &ConnectionIdentity {
                peer_id: viewer.peer_id.clone(),
                connection_id: viewer.connection_id.clone(),
            },
            now,
        );

        let late = now + RoomActorConfig::default().viewer_resume_grace + Duration::seconds(1);
        assert_eq!(
            room.resume(&viewer.resume_token, session().0, late)
                .expect_err("expired token"),
            DomainError::SessionExpired
        );
        // A token that never matched anything stays a plain failure.
        assert_eq!(
            room.resume(&SecretString::from("wrong-token".to_owned()), session().0, now)
                .expect_err("wrong token"),
            DomainError::AuthenticationFailed
        );
    }

    #[test]
    fn display_names_are_normalized_sanitized_and_bounded() {
        assert_eq!(
            sanitize_display_name("  Jamie\u{0}   Example  ").as_deref(),
            Some("Jamie Example")
        );
        assert_eq!(sanitize_display_name(" \n\t "), None);
        assert_eq!(
            sanitize_display_name(&"x".repeat(100)).expect("name").len(),
            48
        );
    }

    #[test]
    fn presenter_and_viewer_authentication_and_approval_are_deterministic() {
        let (mut room, _, presenter_secret, viewer_secret, now) = fixture();
        let presenter = room
            .authenticate_presenter(&presenter_secret, session().0, now)
            .expect("presenter auth");
        let viewer = room
            .authenticate_viewer(&viewer_secret, Some(" Viewer ".into()), None, session().0, now)
            .expect("viewer auth");
        assert_eq!(room.snapshot(now).pending_viewers.len(), 1);
        room.approve(&presenter.peer_id, &viewer.peer_id, "approve".into(), now)
            .expect("approval");
        assert_eq!(room.snapshot(now).pending_viewers.len(), 0);
        assert_eq!(room.snapshot(now).approved_viewers.len(), 1);
    }

    #[test]
    fn public_rooms_auto_admit_invited_viewers_and_enforce_capacity() {
        let (mut room, _, _, viewer_secret, now) = fixture_with_policy(RoomAccessPolicy::Public);
        for index in 0..4 {
            let viewer = room
                .authenticate_viewer(
                    &viewer_secret,
                    Some(format!("Viewer {index}")),
                    None,
                    session().0,
                    now,
                )
                .expect("public viewer");
            assert_eq!(
                viewer
                    .snapshot
                    .approved_viewers
                    .iter()
                    .find(|candidate| candidate.peer_id == viewer.peer_id)
                    .and_then(|candidate| candidate.viewer_state),
                Some(ViewerState::Approved)
            );
        }
        assert!(room.snapshot(now).pending_viewers.is_empty());
        assert_eq!(room.snapshot(now).approved_viewers.len(), 4);
        assert_eq!(
            room.authenticate_viewer(&viewer_secret, None, None, session().0, now)
                .expect_err("fifth viewer is rejected"),
            DomainError::RoomFull
        );
    }

    #[test]
    fn viewers_can_set_their_display_name_after_joining() {
        let (mut room, _, presenter_secret, viewer_secret, now) =
            fixture_with_policy(RoomAccessPolicy::Public);
        let presenter = room
            .authenticate_presenter(&presenter_secret, session().0, now)
            .expect("presenter");
        let (viewer_session, mut viewer_messages) = session();
        let viewer = room
            .authenticate_viewer(&viewer_secret, None, None, viewer_session, now)
            .expect("anonymous viewer");

        room.update_viewer_display_name(
            &viewer.peer_id,
            Some("  Jamie   Viewer  ".into()),
            "rename".into(),
            now,
        )
        .expect("rename");

        assert_eq!(
            room.snapshot(now).approved_viewers[0].display_name.as_deref(),
            Some("Jamie Viewer")
        );
        assert!(matches!(
            viewer_messages.try_recv().expect("rename acknowledgement"),
            ServerMessage::ViewerDisplayNameUpdated { display_name: Some(name), .. }
                if name == "Jamie Viewer"
        ));
        assert_eq!(
            room.update_viewer_display_name(
                &presenter.peer_id,
                Some("Not a viewer".into()),
                "forbidden".into(),
                now,
            ),
            Err(DomainError::AuthorizationDenied)
        );
    }

    #[test]
    fn capacity_can_drop_below_current_count_without_kicking_existing_viewers() {
        let (mut room, _, presenter_secret, viewer_secret, now) = fixture();
        let presenter = room
            .authenticate_presenter(&presenter_secret, session().0, now)
            .expect("presenter");
        for index in 0..2 {
            let viewer = room
                .authenticate_viewer(
                    &viewer_secret,
                    Some(format!("Viewer {index}")),
                    None,
                    session().0,
                    now,
                )
                .expect("viewer");
            room.approve(
                &presenter.peer_id,
                &viewer.peer_id,
                format!("a{index}"),
                now,
            )
            .expect("approve");
        }
        room.update_capacity(&presenter.peer_id, 1, "capacity".into(), now)
            .expect("capacity");
        assert_eq!(room.snapshot(now).approved_viewers.len(), 2);
        let third = room
            .authenticate_viewer(&viewer_secret, None, None, session().0, now)
            .expect("pending allowed");
        assert_eq!(
            room.approve(&presenter.peer_id, &third.peer_id, "third".into(), now),
            Err(DomainError::RoomFull)
        );
    }

    #[test]
    fn sharing_state_is_persisted_and_only_the_presenter_can_update_it() {
        let (mut room, _, presenter_secret, viewer_secret, now) =
            fixture_with_policy(RoomAccessPolicy::Public);
        let presenter = room
            .authenticate_presenter(&presenter_secret, session().0, now)
            .expect("presenter");
        let viewer = room
            .authenticate_viewer(&viewer_secret, None, None, session().0, now)
            .expect("viewer");

        assert_eq!(room.snapshot(now).sharing_state, SharingState::Idle);
        assert_eq!(
            room.update_sharing_state(
                &viewer.peer_id,
                SharingState::Paused,
                "forbidden".into(),
                now,
            ),
            Err(DomainError::AuthorizationDenied)
        );
        room.update_sharing_state(
            &presenter.peer_id,
            SharingState::Paused,
            "pause".into(),
            now,
        )
        .expect("pause");
        assert_eq!(room.snapshot(now).sharing_state, SharingState::Paused);
    }

    #[test]
    fn viewer_to_viewer_and_wrong_direction_signaling_are_rejected() {
        assert_eq!(
            SignalingAuthorizationService::authorize(
                PeerRole::Viewer,
                PeerRole::Viewer,
                &RoutedSignal::IceRestart
            ),
            Err(DomainError::AuthorizationDenied)
        );
        assert_eq!(
            SignalingAuthorizationService::authorize(
                PeerRole::Viewer,
                PeerRole::Presenter,
                &RoutedSignal::Offer {
                    sdp: "x".into(),
                    ice_restart: false
                }
            ),
            Err(DomainError::AuthorizationDenied)
        );
        assert_eq!(
            SignalingAuthorizationService::authorize(
                PeerRole::Viewer,
                PeerRole::Presenter,
                &RoutedSignal::IceRestart
            ),
            Ok(())
        );
    }

    #[test]
    fn expired_rooms_reject_authentication() {
        let (mut room, _, presenter_secret, _, now) = fixture();
        let result =
            room.authenticate_presenter(&presenter_secret, session().0, now + Duration::hours(3));
        assert_eq!(result.expect_err("expired"), DomainError::RoomExpired);
    }

    #[tokio::test]
    async fn actor_removes_closed_room_from_registry() {
        let secrets = Arc::new(SecretDigestService::new(
            SecretString::from("room-key-with-at-least-32-bytes-long".to_owned()),
            SecretString::from("resume-key-with-at-least-32-bytes".to_owned()),
        ));
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("time");
        let clock = Arc::new(ManualClock::new(now));
        let registry = RoomRegistry::with_clock(secrets, RoomActorConfig::default(), clock);
        let created = registry
            .create_room(1, Duration::hours(1), RoomAccessPolicy::Public, Vec::new())
            .await
            .expect("create");
        let (reply_tx, reply_rx) = oneshot::channel();
        registry
            .dispatch(
                &created.room_id,
                RoomCommand::AuthenticatePresenter {
                    credential: created.presenter_secret,
                    session: session().0,
                    reply: reply_tx,
                },
            )
            .await
            .expect("dispatch");
        let presenter = reply_rx.await.expect("reply").expect("auth");
        let (close_tx, close_rx) = oneshot::channel();
        registry
            .dispatch(
                &created.room_id,
                RoomCommand::Close {
                    source: ConnectionIdentity {
                        peer_id: presenter.peer_id,
                        connection_id: presenter.connection_id,
                    },
                    reply: close_tx,
                },
            )
            .await
            .expect("dispatch close");
        close_rx.await.expect("reply").expect("close");
        tokio::task::yield_now().await;
        assert!(registry.is_empty().await);
    }

    #[test]
    fn secret_debug_output_is_redacted() {
        let (room, _, _, _, _) = fixture();
        let output = format!("{room:?}");
        assert!(!output.contains("presenter_digest"));
        assert!(!output.contains(secret_as_str(&SecretString::from("secret".to_owned()))));
    }
}
