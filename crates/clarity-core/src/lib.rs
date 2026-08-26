#![forbid(unsafe_code)]

mod clock;
mod crypto;
mod presence;
mod room;
mod turn;

#[cfg(test)]
pub use clock::ManualClock;
pub use clock::{Clock, SystemClock, ago_compact};
pub use crypto::{GeneratedRoomSecrets, SecretDigest, SecretDigestService, secret_as_str};
pub use presence::{
    PresenceAuthError, PresenceHandle, PresenceRegistry, PresenceUnavailable, SessionId,
    new_challenge, verify_identity,
    verify_identity_for_hosts,
};
pub use room::{
    AuthOutcome, CreateRoomOutcome, DEFAULT_APPROVAL_VIEWERS, DEFAULT_PUBLIC_VIEWERS, DomainError,
    MAXIMUM_VIEWERS_LIMIT, RoomActorConfig, RoomCommand, RoomEvent, RoomRegistry, RoomState,
    RoutedSignal, SessionHandle, SignalingAuthorizationService, sanitize_display_name,
};
pub use turn::{TurnConfig, TurnCredentialService};
