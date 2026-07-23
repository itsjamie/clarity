use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use clarity_protocol::{IceConfiguration, IceServer};
use hmac::{Hmac, Mac};
use secrecy::{ExposeSecret, SecretString};
use sha1::Sha1;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};

type HmacSha1 = Hmac<Sha1>;

#[derive(Clone)]
pub struct TurnConfig {
    pub urls: Vec<String>,
    pub shared_secret: SecretString,
    pub credential_ttl: Duration,
}

impl fmt::Debug for TurnConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TurnConfig")
            .field("urls", &self.urls)
            .field("shared_secret", &"[REDACTED]")
            .field("credential_ttl", &self.credential_ttl)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct TurnCredentialService {
    config: TurnConfig,
}

impl TurnCredentialService {
    #[must_use]
    pub fn new(config: TurnConfig) -> Self {
        Self { config }
    }

    pub fn issue(
        &self,
        peer_id: &str,
        now: OffsetDateTime,
    ) -> Result<IceConfiguration, time::error::Format> {
        let expires_at = now + self.config.credential_ttl;
        let username = format!("{}:{peer_id}", expires_at.unix_timestamp());
        let mut mac =
            HmacSha1::new_from_slice(self.config.shared_secret.expose_secret().as_bytes())
                .unwrap_or_else(|_| unreachable!("HMAC accepts keys of every length"));
        mac.update(username.as_bytes());
        let credential = STANDARD.encode(mac.finalize().into_bytes());

        Ok(IceConfiguration {
            expires_at: expires_at.format(&Rfc3339)?,
            ice_servers: vec![IceServer {
                urls: self.config.urls.clone(),
                username: Some(username),
                credential: Some(credential),
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_coturn_rest_credentials_with_exact_expiry() {
        let service = TurnCredentialService::new(TurnConfig {
            urls: vec!["turn:turn.example.test:3478?transport=udp".into()],
            shared_secret: SecretString::from("shared-secret".to_owned()),
            credential_ttl: Duration::hours(1),
        });
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("valid time");
        let configuration = service.issue("peer-1", now).expect("formats");
        assert_eq!(
            configuration.ice_servers[0].username.as_deref(),
            Some("1700003600:peer-1")
        );
        assert!(
            !configuration.ice_servers[0]
                .credential
                .as_deref()
                .unwrap_or_default()
                .is_empty()
        );
        assert_eq!(configuration.expires_at, "2023-11-14T23:13:20Z");
    }
}
