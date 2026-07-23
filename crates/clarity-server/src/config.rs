use std::{collections::HashSet, env, net::SocketAddr};

use anyhow::{Context, Result, bail};
use clarity_core::{MAXIMUM_VIEWERS_LIMIT, RoomActorConfig, TurnConfig};
use secrecy::{ExposeSecret, SecretString};
use time::Duration;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    Development,
    Production,
}

#[derive(Clone)]
pub struct AppConfig {
    pub environment: Environment,
    pub bind_address: SocketAddr,
    pub public_base_url: Url,
    pub allowed_origins: HashSet<String>,
    pub log_level: String,
    pub default_room_ttl: Duration,
    pub maximum_room_ttl: Duration,
    pub room_actor: RoomActorConfig,
    pub room_token_hmac_key: SecretString,
    pub resume_token_hmac_key: SecretString,
    pub websocket_auth_timeout: std::time::Duration,
    pub websocket_heartbeat_interval: std::time::Duration,
    pub websocket_heartbeat_timeout: std::time::Duration,
    pub websocket_max_message_bytes: usize,
    pub sdp_max_bytes: usize,
    pub ice_candidate_max_bytes: usize,
    pub room_creation_rate_limit: u32,
    pub websocket_connection_rate_limit: u32,
    pub auth_rate_limit: u32,
    pub signal_rate_limit: u32,
    pub turn: TurnConfig,
}

impl std::fmt::Debug for AppConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppConfig")
            .field("environment", &self.environment)
            .field("bind_address", &self.bind_address)
            .field("public_base_url", &self.public_base_url)
            .field("allowed_origins", &self.allowed_origins)
            .field("log_level", &self.log_level)
            .field("default_room_ttl", &self.default_room_ttl)
            .field("maximum_room_ttl", &self.maximum_room_ttl)
            .field("room_actor", &self.room_actor)
            .field("room_token_hmac_key", &"[REDACTED]")
            .field("resume_token_hmac_key", &"[REDACTED]")
            .field("turn", &self.turn)
            .finish_non_exhaustive()
    }
}

impl AppConfig {
    pub fn from_env() -> Result<Self> {
        let environment = match env_string("APP_ENV", "development").as_str() {
            "development" => Environment::Development,
            "production" => Environment::Production,
            value => bail!("APP_ENV must be `development` or `production`, got `{value}`"),
        };
        let bind_address = env_string("APP_BIND_ADDRESS", "127.0.0.1:3000")
            .parse()
            .context("APP_BIND_ADDRESS must be a socket address such as 127.0.0.1:3000")?;
        let public_base_url = Url::parse(&env_string("PUBLIC_BASE_URL", "http://localhost:3000"))
            .context("PUBLIC_BASE_URL must be an absolute URL")?;
        if environment == Environment::Production && public_base_url.scheme() != "https" {
            bail!("PUBLIC_BASE_URL must use https in production");
        }
        let default_origin = public_base_url.origin().ascii_serialization();
        let mut allowed_origins = env_string("ALLOWED_ORIGINS", &default_origin)
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        if environment == Environment::Development {
            allowed_origins.insert("http://localhost:5173".to_owned());
            allowed_origins.insert("http://127.0.0.1:5173".to_owned());
        }

        let default_room_ttl_seconds = env_parse("DEFAULT_ROOM_TTL_SECONDS", 7_200_u64)?;
        let maximum_room_ttl_seconds = env_parse("MAX_ROOM_TTL_SECONDS", 28_800_u64)?;
        if default_room_ttl_seconds == 0 || default_room_ttl_seconds > maximum_room_ttl_seconds {
            bail!(
                "DEFAULT_ROOM_TTL_SECONDS must be positive and no greater than MAX_ROOM_TTL_SECONDS"
            );
        }
        let maximum_viewers_hard_limit =
            env_parse("MAX_VIEWERS_HARD_LIMIT", MAXIMUM_VIEWERS_LIMIT)?;
        if maximum_viewers_hard_limit == 0 || maximum_viewers_hard_limit > MAXIMUM_VIEWERS_LIMIT {
            bail!("MAX_VIEWERS_HARD_LIMIT must be between 1 and 10");
        }

        let room_key = secret_env(
            "ROOM_TOKEN_HMAC_KEY",
            environment,
            "development-room-token-key-change-before-production",
        )?;
        let resume_key = secret_env(
            "RESUME_TOKEN_HMAC_KEY",
            environment,
            "development-resume-token-key-change-before-production",
        )?;
        let turn_secret = secret_env(
            "TURN_SHARED_SECRET",
            environment,
            "development-turn-shared-secret-change-before-production",
        )?;

        let turn_host = env_string("TURN_HOST", "localhost");
        let turn_port = env_parse("TURN_PORT", 3_478_u16)?;
        let turns_port = env_parse("TURNS_PORT", 5_349_u16)?;
        let mut turn_urls = vec![
            format!("stun:{turn_host}:{turn_port}"),
            format!("turn:{turn_host}:{turn_port}?transport=udp"),
            format!("turn:{turn_host}:{turn_port}?transport=tcp"),
        ];
        if env::var("TURN_TLS_CERT_PATH").is_ok() && env::var("TURN_TLS_KEY_PATH").is_ok() {
            turn_urls.push(format!("turns:{turn_host}:{turns_port}?transport=tcp"));
        }

        let config = Self {
            environment,
            bind_address,
            public_base_url,
            allowed_origins,
            log_level: env_string("LOG_LEVEL", "info,clarity_server=debug"),
            default_room_ttl: seconds(default_room_ttl_seconds, "DEFAULT_ROOM_TTL_SECONDS")?,
            maximum_room_ttl: seconds(maximum_room_ttl_seconds, "MAX_ROOM_TTL_SECONDS")?,
            room_actor: RoomActorConfig {
                maximum_viewers_hard_limit,
                maximum_pending_viewers: env_parse("MAX_PENDING_VIEWERS", 16_usize)?,
                pending_viewer_ttl: seconds(
                    env_parse("PENDING_VIEWER_TTL_SECONDS", 120_u64)?,
                    "PENDING_VIEWER_TTL_SECONDS",
                )?,
                presenter_resume_grace: seconds(
                    env_parse("PRESENTER_RESUME_GRACE_SECONDS", 60_u64)?,
                    "PRESENTER_RESUME_GRACE_SECONDS",
                )?,
                viewer_resume_grace: seconds(
                    env_parse("VIEWER_RESUME_GRACE_SECONDS", 60_u64)?,
                    "VIEWER_RESUME_GRACE_SECONDS",
                )?,
                outbound_capacity: env_parse("WS_OUTBOUND_QUEUE_CAPACITY", 128_usize)?,
            },
            room_token_hmac_key: room_key,
            resume_token_hmac_key: resume_key,
            websocket_auth_timeout: std::time::Duration::from_secs(env_parse(
                "WS_AUTH_TIMEOUT_SECONDS",
                10_u64,
            )?),
            websocket_heartbeat_interval: std::time::Duration::from_secs(env_parse(
                "WS_HEARTBEAT_INTERVAL_SECONDS",
                15_u64,
            )?),
            websocket_heartbeat_timeout: std::time::Duration::from_secs(env_parse(
                "WS_HEARTBEAT_TIMEOUT_SECONDS",
                10_u64,
            )?),
            websocket_max_message_bytes: env_parse("WS_MAX_MESSAGE_BYTES", 262_144_usize)?,
            sdp_max_bytes: env_parse("SDP_MAX_BYTES", 131_072_usize)?,
            ice_candidate_max_bytes: env_parse("ICE_CANDIDATE_MAX_BYTES", 4_096_usize)?,
            room_creation_rate_limit: env_parse("ROOM_CREATION_RATE_LIMIT", 10_u32)?,
            websocket_connection_rate_limit: env_parse("JOIN_RATE_LIMIT", 60_u32)?,
            auth_rate_limit: env_parse("AUTH_RATE_LIMIT", 10_u32)?,
            signal_rate_limit: env_parse("SIGNAL_RATE_LIMIT", 240_u32)?,
            turn: TurnConfig {
                urls: turn_urls,
                shared_secret: turn_secret,
                credential_ttl: seconds(
                    env_parse("TURN_CREDENTIAL_TTL_SECONDS", 3_600_u64)?,
                    "TURN_CREDENTIAL_TTL_SECONDS",
                )?,
            },
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.allowed_origins.is_empty() {
            bail!("ALLOWED_ORIGINS must include at least one exact origin");
        }
        for (name, secret) in [
            ("ROOM_TOKEN_HMAC_KEY", &self.room_token_hmac_key),
            ("RESUME_TOKEN_HMAC_KEY", &self.resume_token_hmac_key),
            ("TURN_SHARED_SECRET", &self.turn.shared_secret),
        ] {
            if secret.expose_secret().len() < 32 {
                bail!("{name} must contain at least 32 characters");
            }
        }
        if self.websocket_auth_timeout.is_zero()
            || self.websocket_heartbeat_interval.is_zero()
            || self.websocket_heartbeat_timeout.is_zero()
        {
            bail!("WebSocket timeout and heartbeat settings must be positive");
        }
        Ok(())
    }
}

fn env_string(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn env_parse<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr + std::fmt::Display,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse()
        .with_context(|| format!("{name} has an invalid value"))
}

fn secret_env(
    name: &str,
    environment: Environment,
    development_default: &str,
) -> Result<SecretString> {
    match env::var(name) {
        Ok(value) => Ok(SecretString::from(value)),
        Err(_) if environment == Environment::Development => {
            Ok(SecretString::from(development_default.to_owned()))
        }
        Err(_) => bail!("{name} is required in production"),
    }
}

fn seconds(value: u64, name: &str) -> Result<Duration> {
    let value = i64::try_from(value).with_context(|| format!("{name} is too large"))?;
    Ok(Duration::seconds(value))
}
