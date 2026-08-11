//! The local identity: an Ed25519 key pair plus the names the user presents to
//! friends. The private key lives only on this device (persisted by [`Store`]);
//! the public key yields the shareable [friend code](crate::code).
//!
//! [`Store`]: crate::Store

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};

use crate::now_unix;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("a new identity key could not be generated")]
    Generate,
    #[error("the stored identity is corrupt or unreadable")]
    Corrupt,
}

/// A device-local identity. Cloneable so callers can hold a snapshot; the clone
/// carries the same key material.
#[derive(Clone)]
pub struct Identity {
    /// PKCS#8 v2 document holding the Ed25519 private key. Never serialized in
    /// the clear beyond this machine's config directory.
    pkcs8: Vec<u8>,
    public_key: [u8; 32],
    display_name: String,
    device_name: String,
    created_at: u64,
}

impl Identity {
    /// Generates a fresh identity for this device.
    pub fn create(
        display_name: impl Into<String>,
        device_name: impl Into<String>,
    ) -> Result<Self, IdentityError> {
        let pkcs8 = generate_pkcs8()?;
        Self::from_parts(pkcs8, display_name.into(), device_name.into(), now_unix())
    }

    /// The friend code others enter to add this identity.
    pub fn friend_code(&self) -> String {
        crate::code::encode(&self.public_key)
    }

    /// The 32-byte Ed25519 public key, shared with a server to prove identity.
    pub fn public_key(&self) -> [u8; 32] {
        self.public_key
    }

    /// Signs `message` with the identity's private key (Ed25519, 64-byte
    /// signature), for the presence handshake challenge.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        let key_pair =
            Ed25519KeyPair::from_pkcs8(&self.pkcs8).expect("stored identity key is valid");
        let mut signature = [0u8; 64];
        signature.copy_from_slice(key_pair.sign(message).as_ref());
        signature
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn set_display_name(&mut self, name: impl Into<String>) {
        self.display_name = name.into();
    }

    pub fn set_device_name(&mut self, name: impl Into<String>) {
        self.device_name = name.into();
    }

    /// Replaces the key pair, keeping the names. The old friend code stops
    /// resolving; contacts already added are unaffected (they hold their own
    /// codes, not this one).
    pub fn rotate(&mut self) -> Result<(), IdentityError> {
        let pkcs8 = generate_pkcs8()?;
        self.public_key = public_key_of(&pkcs8)?;
        self.pkcs8 = pkcs8;
        Ok(())
    }

    fn from_parts(
        pkcs8: Vec<u8>,
        display_name: String,
        device_name: String,
        created_at: u64,
    ) -> Result<Self, IdentityError> {
        let public_key = public_key_of(&pkcs8)?;
        Ok(Self {
            pkcs8,
            public_key,
            display_name,
            device_name,
            created_at,
        })
    }

    pub(crate) fn to_stored(&self) -> StoredIdentity {
        StoredIdentity {
            key: BASE64.encode(&self.pkcs8),
            display_name: self.display_name.clone(),
            device_name: self.device_name.clone(),
            created_at: self.created_at,
        }
    }

    pub(crate) fn from_stored(stored: StoredIdentity) -> Result<Self, IdentityError> {
        let pkcs8 = BASE64
            .decode(stored.key.as_bytes())
            .map_err(|_| IdentityError::Corrupt)?;
        Self::from_parts(
            pkcs8,
            stored.display_name,
            stored.device_name,
            stored.created_at,
        )
    }
}

/// On-disk shape of an identity. The key is base64 PKCS#8.
#[derive(Serialize, Deserialize)]
pub(crate) struct StoredIdentity {
    key: String,
    display_name: String,
    device_name: String,
    created_at: u64,
}

fn generate_pkcs8() -> Result<Vec<u8>, IdentityError> {
    let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| IdentityError::Generate)?;
    Ok(document.as_ref().to_vec())
}

fn public_key_of(pkcs8: &[u8]) -> Result<[u8; 32], IdentityError> {
    let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| IdentityError::Corrupt)?;
    let mut public_key = [0u8; 32];
    public_key.copy_from_slice(key_pair.public_key().as_ref());
    Ok(public_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_storage() {
        let identity = Identity::create("Jamie", "Studio Mac").expect("create");
        let code = identity.friend_code();
        let restored = Identity::from_stored(identity.to_stored()).expect("reload");
        assert_eq!(restored.display_name(), "Jamie");
        assert_eq!(restored.device_name(), "Studio Mac");
        assert_eq!(restored.friend_code(), code);
    }

    #[test]
    fn rotate_changes_the_code_but_keeps_names() {
        let mut identity = Identity::create("Jamie", "Studio Mac").expect("create");
        let before = identity.friend_code();
        identity.rotate().expect("rotate");
        assert_ne!(identity.friend_code(), before);
        assert_eq!(identity.display_name(), "Jamie");
    }
}
