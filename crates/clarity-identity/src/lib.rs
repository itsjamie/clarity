//! Device-local identity, contacts, and settings for the Clarity desktop
//! client — the state that lives on this machine and needs no server.
//!
//! An [`Identity`] is an Ed25519 key pair; its public half yields a short
//! [friend `code`](code) other people enter to add you. [`Contacts`] is the
//! list of friends so added. [`Settings`] is the user's client configuration.
//! [`Store`] persists all three under the user's config directory.
//!
//! Presence — who is online or sharing — is deliberately absent here; it
//! belongs to the signaling server and arrives with that work.

#![forbid(unsafe_code)]

/// Friend-code derivation and parsing, shared with the wire protocol.
pub use clarity_protocol::code;

mod contacts;
mod identity;
mod settings;
mod store;

pub use contacts::{Contact, ContactError, Contacts};
pub use identity::{Identity, IdentityError};
pub use settings::{CaptureProfile, Settings};
pub use store::{Store, StoreError};

/// Seconds since the Unix epoch, saturating at 0 before it. Used for the
/// `created_at`/`added_at` timestamps on identities and contacts.
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
