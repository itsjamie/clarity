//! The local contact list: friends added by code. Presence is not modeled here
//! — that arrives with the signaling server — so a contact is `pending` until a
//! future exchange confirms it, then active.

use serde::{Deserialize, Serialize};

use crate::now_unix;

/// How long an unaccepted invite lives, mirroring the server's request TTL
/// so the invite disappears for both sides on roughly the same clock.
pub const INVITE_TTL_SECONDS: u64 = 10 * 60;

#[derive(Debug, thiserror::Error)]
pub enum ContactError {
    #[error("that is not a valid friend code")]
    InvalidCode,
    #[error("that is your own code")]
    IsSelf,
    #[error("that friend is already added")]
    AlreadyAdded,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Contact {
    /// Canonical friend code (`clr-XXXX-XXXX`).
    pub code: String,
    /// The local, private label for this friend.
    pub name: String,
    /// Unix seconds when this contact was added. Defaults to 0 so contact
    /// files written before the field existed still load.
    #[serde(default)]
    pub added_at: u64,
    /// True until the friend confirms the trade; presence support will clear it.
    pub pending: bool,
}

impl Contact {
    /// Seconds since this contact was added. `None` when the record predates
    /// the `added_at` field (deserialized as the 0 default), where any figure
    /// would be a guess.
    pub fn added_seconds_ago(&self) -> Option<u64> {
        (self.added_at > 0).then(|| now_unix().saturating_sub(self.added_at))
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Contacts {
    contacts: Vec<Contact>,
}

impl Contacts {
    /// Adds a friend by code. The code is normalized first; `own_code` is
    /// rejected so a user cannot add themselves. New contacts start `pending`.
    pub fn add(&mut self, code: &str, name: &str, own_code: &str) -> Result<(), ContactError> {
        let code = crate::code::normalize(code).ok_or(ContactError::InvalidCode)?;
        if code == own_code {
            return Err(ContactError::IsSelf);
        }
        if self.contacts.iter().any(|c| c.code == code) {
            return Err(ContactError::AlreadyAdded);
        }
        self.contacts.push(Contact {
            code,
            name: name.trim().to_owned(),
            added_at: now_unix(),
            pending: true,
        });
        Ok(())
    }

    pub fn remove(&mut self, code: &str) {
        self.contacts.retain(|c| c.code != code);
    }

    /// Marks a contact as confirmed (no longer pending).
    pub fn confirm(&mut self, code: &str) {
        if let Some(contact) = self.contacts.iter_mut().find(|c| c.code == code) {
            contact.pending = false;
        }
    }

    /// Drops pending contacts whose invite has aged out
    /// ([`INVITE_TTL_SECONDS`]); `true` when any were dropped. Adding a
    /// friend is something both people do in the moment, so an unanswered
    /// invite disappears instead of waiting forever; removing the contact
    /// also shrinks the presence subscription, withdrawing the request
    /// server-side. A pending contact with no recorded `added_at` (older
    /// data) is left alone rather than dropped on an unknown age.
    pub fn expire_invites(&mut self) -> bool {
        let before = self.contacts.len();
        self.contacts.retain(|contact| {
            !contact.pending
                || contact
                    .added_seconds_ago()
                    .is_none_or(|age| age < INVITE_TTL_SECONDS)
        });
        self.contacts.len() != before
    }

    pub fn iter(&self) -> impl Iterator<Item = &Contact> {
        self.contacts.iter()
    }

    /// The local name for a friend code, if it is a contact.
    pub fn name_of(&self, code: &str) -> Option<&str> {
        self.contacts
            .iter()
            .find(|c| c.code == code)
            .map(|c| c.name.as_str())
    }

    /// Contacts still awaiting confirmation ("Waiting on them").
    pub fn pending(&self) -> impl Iterator<Item = &Contact> {
        self.contacts.iter().filter(|c| c.pending)
    }

    /// Confirmed contacts.
    pub fn active(&self) -> impl Iterator<Item = &Contact> {
        self.contacts.iter().filter(|c| !c.pending)
    }

    pub fn is_empty(&self) -> bool {
        self.contacts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.contacts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: &str = "clr-AAAA-BBBB";

    /// A real, base32-valid friend code (the design's `clr-8QF2-NKD7` sample is
    /// illustrative only — `8` is not in the base32 alphabet).
    fn a_code() -> String {
        crate::code::encode(&[5u8; 32])
    }

    #[test]
    fn expires_only_aged_pending_invites() {
        let mut contacts = Contacts::default();
        let aged = now_unix() - INVITE_TTL_SECONDS - 1;
        contacts.contacts = vec![
            // An unanswered invite past the TTL: dropped.
            Contact {
                code: "clr-OLD1-OLD1".into(),
                name: "Aged".into(),
                added_at: aged,
                pending: true,
            },
            // A confirmed friend, however old: kept.
            Contact {
                code: "clr-OLD2-OLD2".into(),
                name: "Friend".into(),
                added_at: aged,
                pending: false,
            },
            // A fresh invite: kept.
            Contact {
                code: "clr-NEW1-NEW1".into(),
                name: "Fresh".into(),
                added_at: now_unix(),
                pending: true,
            },
            // Pending with no recorded age (pre-`added_at` data): kept.
            Contact {
                code: "clr-NOAG-NOAG".into(),
                name: "Legacy".into(),
                added_at: 0,
                pending: true,
            },
        ];
        assert!(contacts.expire_invites());
        let codes: Vec<&str> = contacts.iter().map(|c| c.code.as_str()).collect();
        assert_eq!(codes, ["clr-OLD2-OLD2", "clr-NEW1-NEW1", "clr-NOAG-NOAG"]);
        assert!(!contacts.expire_invites());
    }

    #[test]
    fn add_normalizes_and_dedupes() {
        let mut contacts = Contacts::default();
        let code = a_code();
        contacts.add(&code, "Mara", OWN).expect("add");
        assert_eq!(contacts.len(), 1);
        // Same code, messier spelling: rejected as already added.
        let messy = code["clr-".len()..].to_ascii_lowercase().replace('-', " ");
        let err = contacts.add(&messy, "Mara again", OWN).unwrap_err();
        assert!(matches!(err, ContactError::AlreadyAdded));
    }

    #[test]
    fn rejects_self_and_garbage() {
        let mut contacts = Contacts::default();
        assert!(matches!(
            contacts.add(OWN, "me", OWN),
            Err(ContactError::IsSelf)
        ));
        assert!(matches!(
            contacts.add("clr-0000-0000", "x", OWN),
            Err(ContactError::InvalidCode)
        ));
    }

    #[test]
    fn tolerates_contacts_saved_before_added_at_existed() {
        let json = format!(
            r#"[{{"code":"{}","name":"Mara","pending":true}}]"#,
            a_code()
        );
        let contacts: Contacts = serde_json::from_str(&json).expect("deserialize");
        let contact = contacts.iter().next().expect("one contact");
        assert_eq!(contact.added_at, 0);
        assert_eq!(contact.added_seconds_ago(), None);
    }

    #[test]
    fn added_seconds_ago_counts_from_added_at() {
        let mut contacts = Contacts::default();
        contacts.add(&a_code(), "Mara", OWN).expect("add");
        let ago = contacts
            .iter()
            .next()
            .and_then(|c| c.added_seconds_ago())
            .expect("known age");
        assert!(ago < 60, "a just-added contact should read as seconds old");
    }

    #[test]
    fn partitions_pending_and_active() {
        let mut contacts = Contacts::default();
        contacts.add(&a_code(), "Mara", OWN).expect("add");
        assert_eq!(contacts.pending().count(), 1);
        assert_eq!(contacts.active().count(), 0);
    }
}
