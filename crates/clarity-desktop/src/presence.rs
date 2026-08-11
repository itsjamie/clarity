//! The background presence connection for the GUI.
//!
//! [`PresenceLink`] owns a tokio runtime and a [`PresenceSession`], bridging its
//! async events onto the egui thread: each event is forwarded through a plain
//! channel and a repaint is requested, so the sync render loop can drain them
//! into [`PresenceView`] without blocking. Subscriptions track the contact list.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use clarity_client::presence::{PresenceConfig, PresenceEvent, PresenceSession, PresenceState};
use clarity_identity::Store;

use crate::state::PresenceView;

pub struct PresenceLink {
    session: PresenceSession,
    incoming: Receiver<PresenceEvent>,
    /// The codes currently subscribed, so [`sync`](Self::sync) only resubscribes
    /// when the contact set actually changes.
    subscribed: Vec<String>,
}

impl PresenceLink {
    /// Starts a presence connection for the store's identity on the given
    /// runtime. Returns `None` when there is no identity or the server URL
    /// cannot be parsed — presence is best-effort and never blocks the UI.
    pub fn start(
        runtime: &tokio::runtime::Handle,
        ctx: &egui::Context,
        store: &Store,
    ) -> Option<Self> {
        let identity = store.identity.as_ref()?;
        let (url, origin) = presence_endpoint(&store.settings.signaling_server)?;

        let signing_identity = identity.clone();
        let config = PresenceConfig {
            url,
            origin,
            public_key: identity.public_key().to_vec(),
            sign: Arc::new(move |message: &[u8]| signing_identity.sign(message)),
        };

        let (session, mut events) = {
            let _guard = runtime.enter();
            PresenceSession::connect(config)
        };

        let (forward, incoming) = channel();
        let repaint = ctx.clone();
        runtime.spawn(async move {
            while let Some(event) = events.recv().await {
                if forward.send(event).is_err() {
                    break;
                }
                repaint.request_repaint();
            }
        });

        let mut link = Self {
            session,
            incoming,
            subscribed: Vec::new(),
        };
        link.sync(contact_codes(store));
        Some(link)
    }

    /// Announces the room being hosted now (or `None` when sharing stops), so
    /// mutually-added friends see it in their "Live now". The announcement
    /// carries the presenter secret so the server can verify this app hosts
    /// the room it claims.
    pub fn announce(&self, hosting: Option<clarity_client::presence::HostingAnnouncement>) {
        self.session.announce(hosting);
    }

    /// Drains pending presence events into `view`. Cheap to call every frame.
    pub fn pump(&mut self, view: &mut PresenceView) {
        while let Ok(event) = self.incoming.try_recv() {
            match event {
                PresenceEvent::State(state) => {
                    view.connected = matches!(state, PresenceState::Connected);
                }
                PresenceEvent::Ready { code } => view.self_code = Some(code),
                PresenceEvent::Snapshot(friends) => {
                    view.friends = friends
                        .into_iter()
                        .map(|friend| (friend.code.clone(), friend))
                        .collect();
                }
                PresenceEvent::Update(friend) => {
                    view.friends.insert(friend.code.clone(), friend);
                }
            }
        }
    }

    /// Ensures the subscription matches `codes` (the current contacts).
    pub fn sync(&mut self, mut codes: Vec<String>) {
        codes.sort();
        codes.dedup();
        if codes != self.subscribed {
            self.session.subscribe(codes.clone());
            self.subscribed = codes;
        }
    }
}

pub fn contact_codes(store: &Store) -> Vec<String> {
    store.contacts.iter().map(|c| c.code.clone()).collect()
}

/// Derives the presence WebSocket URL and Origin header from the configured
/// signaling server (an `http`/`https` base URL).
fn presence_endpoint(server: &str) -> Option<(String, String)> {
    let origin = server.trim().trim_end_matches('/').to_owned();
    let ws = match origin.split_once("://") {
        Some(("https", rest)) => format!("wss://{rest}"),
        Some(("http", rest)) => format!("ws://{rest}"),
        _ => return None,
    };
    Some((format!("{ws}/api/v1/presence"), origin))
}
