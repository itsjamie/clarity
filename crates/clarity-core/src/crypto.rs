use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::{RngCore, rngs::OsRng};
use secrecy::{ExposeSecret, SecretString};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

const ROOM_ID_BYTES: usize = 12;
const SECRET_BYTES: usize = 32;
const PEER_ID_BYTES: usize = 12;

#[derive(Clone)]
pub struct SecretDigestService {
    room_token_key: SecretString,
    resume_token_key: SecretString,
}

impl fmt::Debug for SecretDigestService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretDigestService([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SecretDigest([u8; 32]);

impl fmt::Debug for SecretDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretDigest([REDACTED])")
    }
}

pub struct GeneratedRoomSecrets {
    pub room_id: String,
    pub presenter_secret: SecretString,
    pub viewer_secret: SecretString,
}

impl fmt::Debug for GeneratedRoomSecrets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GeneratedRoomSecrets")
            .field("room_id", &self.room_id)
            .field("presenter_secret", &"[REDACTED]")
            .field("viewer_secret", &"[REDACTED]")
            .finish()
    }
}

impl SecretDigestService {
    #[must_use]
    pub fn new(room_token_key: SecretString, resume_token_key: SecretString) -> Self {
        Self {
            room_token_key,
            resume_token_key,
        }
    }

    #[must_use]
    pub fn generate_room_secrets(&self) -> GeneratedRoomSecrets {
        GeneratedRoomSecrets {
            room_id: random_token(ROOM_ID_BYTES),
            presenter_secret: SecretString::from(random_token(SECRET_BYTES)),
            viewer_secret: SecretString::from(random_token(SECRET_BYTES)),
        }
    }

    #[must_use]
    pub fn generate_peer_id(&self) -> String {
        random_token(PEER_ID_BYTES)
    }

    #[must_use]
    pub fn generate_resume_token(&self) -> SecretString {
        SecretString::from(random_token(SECRET_BYTES))
    }

    #[must_use]
    pub fn presenter_digest(&self, secret: &SecretString) -> SecretDigest {
        self.digest(
            self.room_token_key.expose_secret().as_bytes(),
            b"presenter\0",
            secret,
        )
    }

    #[must_use]
    pub fn viewer_digest(&self, secret: &SecretString) -> SecretDigest {
        self.digest(
            self.room_token_key.expose_secret().as_bytes(),
            b"viewer\0",
            secret,
        )
    }

    #[must_use]
    pub fn resume_digest(&self, secret: &SecretString) -> SecretDigest {
        self.digest(
            self.resume_token_key.expose_secret().as_bytes(),
            b"resume\0",
            secret,
        )
    }

    #[must_use]
    pub fn verify(&self, expected: &SecretDigest, supplied: &SecretDigest) -> bool {
        bool::from(expected.0.ct_eq(&supplied.0))
    }

    fn digest(&self, key: &[u8], label: &[u8], secret: &SecretString) -> SecretDigest {
        let mut mac = HmacSha256::new_from_slice(key)
            .unwrap_or_else(|_| unreachable!("HMAC accepts keys of every length"));
        mac.update(label);
        mac.update(secret.expose_secret().as_bytes());
        SecretDigest(mac.finalize().into_bytes().into())
    }
}

#[must_use]
pub fn secret_as_str(secret: &SecretString) -> &str {
    secret.expose_secret()
}

fn random_token(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;

    use super::*;

    fn service() -> SecretDigestService {
        SecretDigestService::new(
            SecretString::from("room-key-with-at-least-32-bytes-long".to_owned()),
            SecretString::from("resume-key-with-at-least-32-bytes".to_owned()),
        )
    }

    #[test]
    fn generated_values_have_required_entropy_lengths_and_are_unique() {
        let service = service();
        let mut room_ids = HashSet::new();
        let mut secrets = HashSet::new();
        for _ in 0..128 {
            let generated = service.generate_room_secrets();
            assert_eq!(generated.room_id.len(), 16);
            assert!(secret_as_str(&generated.presenter_secret).len() >= 43);
            assert!(secret_as_str(&generated.viewer_secret).len() >= 43);
            assert!(room_ids.insert(generated.room_id));
            assert!(secrets.insert(secret_as_str(&generated.presenter_secret).to_owned()));
        }
    }

    #[test]
    fn digest_domains_are_separated_and_compare_in_constant_time() {
        let service = service();
        let secret = SecretString::from("same-secret".to_owned());
        let presenter = service.presenter_digest(&secret);
        let viewer = service.viewer_digest(&secret);
        let resume = service.resume_digest(&secret);
        assert_ne!(presenter, viewer);
        assert_ne!(viewer, resume);
        assert!(service.verify(&presenter, &service.presenter_digest(&secret)));
        assert!(!service.verify(&presenter, &viewer));
    }

    proptest! {
        #[test]
        fn public_identifiers_are_url_safe(_seed in any::<u64>()) {
            let id = service().generate_room_secrets().room_id;
            prop_assert!(id.chars().all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_'));
        }
    }
}
