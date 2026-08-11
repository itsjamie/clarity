//! On-disk home for the client's local state. The store owns *where* and *how*
//! identity, contacts, and settings persist; callers mutate the in-memory
//! values and ask the store to persist the matching one.
//!
//! Files live under `$CLARITY_CONFIG_DIR`, else `$XDG_CONFIG_HOME/clarity`,
//! else `~/.config/clarity`. Writes are atomic (temp file + rename); the
//! identity file is written `0600` since it holds the private key.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::identity::StoredIdentity;
use crate::{Contacts, Identity, Settings};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("the config directory could not be located")]
    NoConfigDir,
    #[error("could not access {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is corrupt and could not be read")]
    Corrupt { path: PathBuf },
}

const IDENTITY_FILE: &str = "identity.json";
const CONTACTS_FILE: &str = "contacts.json";
const SETTINGS_FILE: &str = "settings.json";

pub struct Store {
    dir: PathBuf,
    /// `None` until the user creates one during onboarding.
    pub identity: Option<Identity>,
    pub contacts: Contacts,
    pub settings: Settings,
}

impl Store {
    /// Opens the store, creating the config directory if needed. A missing file
    /// is the normal first-run state and loads as a default (identity `None`);
    /// a present-but-unreadable file is an error, so a typo or disk fault is
    /// surfaced rather than silently discarding a user's identity.
    pub fn open() -> Result<Self, StoreError> {
        let dir = config_dir()?;
        fs::create_dir_all(&dir).map_err(|source| StoreError::Io {
            path: dir.clone(),
            source,
        })?;

        let identity = match read_json::<StoredIdentity>(&dir.join(IDENTITY_FILE))? {
            Some(stored) => Some(Identity::from_stored(stored).map_err(|_| StoreError::Corrupt {
                path: dir.join(IDENTITY_FILE),
            })?),
            None => None,
        };
        let contacts = read_json(&dir.join(CONTACTS_FILE))?.unwrap_or_default();
        let settings = read_json(&dir.join(SETTINGS_FILE))?.unwrap_or_default();

        Ok(Self {
            dir,
            identity,
            contacts,
            settings,
        })
    }

    pub fn persist_identity(&self) -> Result<(), StoreError> {
        let path = self.dir.join(IDENTITY_FILE);
        match &self.identity {
            Some(identity) => write_json(&path, &identity.to_stored(), true),
            None => remove(&path),
        }
    }

    pub fn persist_contacts(&self) -> Result<(), StoreError> {
        write_json(&self.dir.join(CONTACTS_FILE), &self.contacts, false)
    }

    pub fn persist_settings(&self) -> Result<(), StoreError> {
        write_json(&self.dir.join(SETTINGS_FILE), &self.settings, false)
    }
}

fn config_dir() -> Result<PathBuf, StoreError> {
    if let Some(dir) = std::env::var_os("CLARITY_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(xdg).join("clarity"));
    }
    let home = std::env::var_os("HOME").ok_or(StoreError::NoConfigDir)?;
    Ok(PathBuf::from(home).join(".config").join("clarity"))
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, StoreError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(StoreError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| StoreError::Corrupt {
            path: path.to_owned(),
        })
}

fn write_json<T: Serialize>(path: &Path, value: &T, private: bool) -> Result<(), StoreError> {
    let body = serde_json::to_vec_pretty(value).map_err(|_| StoreError::Corrupt {
        path: path.to_owned(),
    })?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, &body).map_err(|source| StoreError::Io {
        path: tmp.clone(),
        source,
    })?;
    if private {
        restrict_permissions(&tmp)?;
    }
    fs::rename(&tmp, path).map_err(|source| StoreError::Io {
        path: path.to_owned(),
        source,
    })
}

fn remove(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StoreError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| StoreError::Io {
        path: path.to_owned(),
        source,
    })
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}
