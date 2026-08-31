use secrecy::SecretString;
use url::Url;

use crate::url_authority;

/// A parsed viewer invitation. The secret never appears in `Debug` output and
/// must not be logged; it leaves this struct only inside the authentication
/// message.
pub struct Invitation {
    pub room_id: String,
    pub secret: SecretString,
    /// WebSocket signaling endpoint derived from the invitation's host.
    pub signaling_url: String,
    /// Origin the server expects on the upgrade request — the invitation's own
    /// web origin, since the server allowlists exactly the origins it serves.
    pub origin: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvitationError {
    #[error("the invitation is not a valid URL")]
    Invalid,
    #[error("the invitation must be an http or https link")]
    UnsupportedScheme,
    #[error("the invitation path is incomplete; expected a link like https://host/r/<room>")]
    UnrecognizedPath,
    #[error(
        "the invitation secret is missing; copy the complete viewer link, including everything after `#`"
    )]
    MissingSecret,
}

/// Parses a viewer invitation URL of the form
/// `https://host/r/<roomId>[?access=public]#<viewerSecret>`.
pub fn parse_invitation(input: &str) -> Result<Invitation, InvitationError> {
    let url = Url::parse(input.trim()).map_err(|_| InvitationError::Invalid)?;
    let ws_scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => return Err(InvitationError::UnsupportedScheme),
    };
    let authority = url_authority(&url).ok_or(InvitationError::Invalid)?;
    let mut segments = url
        .path_segments()
        .ok_or(InvitationError::UnrecognizedPath)?
        .filter(|segment| !segment.is_empty());
    let (Some("r"), Some(room_id), None) = (segments.next(), segments.next(), segments.next())
    else {
        return Err(InvitationError::UnrecognizedPath);
    };
    let secret = url
        .fragment()
        .filter(|fragment| !fragment.is_empty())
        .ok_or(InvitationError::MissingSecret)?;
    Ok(Invitation {
        room_id: room_id.to_owned(),
        secret: SecretString::from(secret.to_owned()),
        signaling_url: format!("{ws_scheme}://{authority}/api/v1/ws"),
        origin: format!("{}://{authority}", url.scheme()),
    })
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::*;

    #[test]
    fn parses_a_production_invitation() {
        let invitation =
            parse_invitation("https://share.example.com/r/abc123?access=public#s3cr3t").unwrap();
        assert_eq!(invitation.room_id, "abc123");
        assert_eq!(invitation.secret.expose_secret(), "s3cr3t");
        assert_eq!(
            invitation.signaling_url,
            "wss://share.example.com/api/v1/ws"
        );
        assert_eq!(invitation.origin, "https://share.example.com");
    }

    #[test]
    fn parses_a_local_development_invitation() {
        let invitation = parse_invitation("http://127.0.0.1:5173/r/room#secret").unwrap();
        assert_eq!(invitation.signaling_url, "ws://127.0.0.1:5173/api/v1/ws");
        assert_eq!(invitation.origin, "http://127.0.0.1:5173");
    }

    #[test]
    fn preserves_ipv6_brackets_in_connection_endpoints() {
        let invitation = parse_invitation("http://[::1]:5173/r/room#secret").unwrap();
        assert_eq!(invitation.signaling_url, "ws://[::1]:5173/api/v1/ws");
        assert_eq!(invitation.origin, "http://[::1]:5173");
    }

    #[test]
    fn rejects_incomplete_invitations() {
        assert!(matches!(
            parse_invitation("not a url"),
            Err(InvitationError::Invalid)
        ));
        assert!(matches!(
            parse_invitation("ftp://host/r/room#s"),
            Err(InvitationError::UnsupportedScheme)
        ));
        assert!(matches!(
            parse_invitation("https://host/watch/room#s"),
            Err(InvitationError::UnrecognizedPath)
        ));
        assert!(matches!(
            parse_invitation("https://host/r/room"),
            Err(InvitationError::MissingSecret)
        ));
        assert!(matches!(
            parse_invitation("https://host/r/room#"),
            Err(InvitationError::MissingSecret)
        ));
    }
}
