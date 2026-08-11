use clarity_protocol::IceConfiguration;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

/// ICE server endpoints in the `scheme://user:pass@host` form the media stack
/// consumes, translated from the `scheme:host` form used on the wire (RFC 7064
/// and RFC 7065, where credentials travel out of band).
pub(crate) struct IceEndpoints {
    pub stun_server: Option<String>,
    pub turn_servers: Vec<String>,
}

pub(crate) fn ice_endpoints(configuration: &IceConfiguration) -> IceEndpoints {
    let mut stun_server = None;
    let mut turn_servers = Vec::new();
    for server in &configuration.ice_servers {
        for url in &server.urls {
            if let Some(rest) = url.strip_prefix("stun:") {
                if stun_server.is_none() {
                    stun_server = Some(format!("stun://{rest}"));
                }
                continue;
            }
            let Some((scheme @ ("turn" | "turns"), rest)) = url.split_once(':') else {
                continue;
            };
            let credentials = match (&server.username, &server.credential) {
                (Some(username), Some(credential)) => {
                    format!("{}:{}@", encode(username), encode(credential))
                }
                _ => String::new(),
            };
            turn_servers.push(format!("{scheme}://{credentials}{rest}"));
        }
    }
    IceEndpoints {
        stun_server,
        turn_servers,
    }
}

fn encode(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use clarity_protocol::IceServer;

    use super::*;

    fn configuration(servers: Vec<IceServer>) -> IceConfiguration {
        IceConfiguration {
            expires_at: "2026-01-01T00:00:00Z".into(),
            ice_servers: servers,
        }
    }

    #[test]
    fn translates_stun_and_turn_urls() {
        let endpoints = ice_endpoints(&configuration(vec![
            IceServer {
                urls: vec!["stun:stun.example.com:3478".into()],
                username: None,
                credential: None,
            },
            IceServer {
                urls: vec!["turn:relay.example.com:3478?transport=udp".into()],
                username: Some("1700000000:clarity".into()),
                credential: Some("se/cr+et=".into()),
            },
        ]));

        assert_eq!(
            endpoints.stun_server.as_deref(),
            Some("stun://stun.example.com:3478")
        );
        assert_eq!(
            endpoints.turn_servers,
            vec![
                "turn://1700000000%3Aclarity:se%2Fcr%2Bet%3D@relay.example.com:3478?transport=udp"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn ignores_unknown_schemes_and_keeps_first_stun() {
        let endpoints = ice_endpoints(&configuration(vec![IceServer {
            urls: vec![
                "stun:a.example.com".into(),
                "stun:b.example.com".into(),
                "http://not-ice.example.com".into(),
            ],
            username: None,
            credential: None,
        }]));

        assert_eq!(
            endpoints.stun_server.as_deref(),
            Some("stun://a.example.com")
        );
        assert!(endpoints.turn_servers.is_empty());
    }
}
