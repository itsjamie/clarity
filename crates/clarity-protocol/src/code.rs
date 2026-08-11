//! Friend codes: a short, human-tradeable fingerprint of an identity's public
//! key, formatted `clr-XXXX-XXXX`.
//!
//! The code is the first 40 bits of SHA-256 over the 32-byte Ed25519 public
//! key, in RFC 4648 base32 (A–Z, 2–7). It identifies a peer for the trade-codes
//! flow; it is not the key itself, so it cannot be reversed into one.

use data_encoding::BASE32_NOPAD;
use sha2::{Digest, Sha256};

const PREFIX: &str = "clr";
const BODY_LEN: usize = 8;

/// The canonical friend code for a public key, e.g. `clr-8QF2-NKD7`.
pub fn encode(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    let body = BASE32_NOPAD.encode(&digest[..5]);
    format!("{PREFIX}-{}-{}", &body[..4], &body[4..8])
}

/// Parses a user-entered code into canonical form, tolerating case, spaces, a
/// missing `clr` prefix, and missing or extra dashes. Returns `None` if the
/// body is not exactly eight base32 characters.
pub fn normalize(input: &str) -> Option<String> {
    let cleaned: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let body = cleaned
        .strip_prefix(&PREFIX.to_ascii_uppercase())
        .unwrap_or(&cleaned);
    if body.len() != BODY_LEN || !body.chars().all(is_base32) {
        return None;
    }
    Some(format!("{PREFIX}-{}-{}", &body[..4], &body[4..8]))
}

/// Whether `input` names a well-formed friend code.
pub fn is_valid(input: &str) -> bool {
    normalize(input).is_some()
}

fn is_base32(c: char) -> bool {
    c.is_ascii_uppercase() || ('2'..='7').contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_is_stable_and_well_formed() {
        let key = [7u8; 32];
        let code = encode(&key);
        assert_eq!(code, encode(&key));
        assert!(code.starts_with("clr-"));
        assert_eq!(code.len(), "clr-XXXX-XXXX".len());
        assert!(is_valid(&code));
    }

    #[test]
    fn different_keys_give_different_codes() {
        assert_ne!(encode(&[1u8; 32]), encode(&[2u8; 32]));
    }

    #[test]
    fn normalize_accepts_messy_input() {
        let canonical = encode(&[9u8; 32]);
        let body = &canonical["clr-".len()..];
        let messy = format!("  {}  ", body.to_ascii_lowercase().replace('-', " "));
        assert_eq!(normalize(&messy).as_deref(), Some(canonical.as_str()));
        assert_eq!(normalize(&canonical).as_deref(), Some(canonical.as_str()));
    }

    #[test]
    fn normalize_rejects_wrong_length_and_alphabet() {
        assert_eq!(normalize("clr-abc"), None);
        assert_eq!(normalize("clr-0000-1111"), None); // 0 and 1 are not base32
    }
}
