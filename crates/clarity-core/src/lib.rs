#![forbid(unsafe_code)]

mod clock;
mod crypto;
mod room;
mod turn;

#[cfg(test)]
pub use clock::ManualClock;
pub use clock::{Clock, SystemClock};
pub use crypto::{GeneratedRoomSecrets, SecretDigest, SecretDigestService, secret_as_str};
pub use room::{
    AuthOutcome, CreateRoomOutcome, DEFAULT_APPROVAL_VIEWERS, DEFAULT_PUBLIC_VIEWERS, DomainError,
    MAXIMUM_VIEWERS_LIMIT, RoomActorConfig, RoomCommand, RoomRegistry, RoomState, RoutedSignal,
    SessionHandle, SignalingAuthorizationService, sanitize_display_name,
};
pub use turn::{TurnConfig, TurnCredentialService};
