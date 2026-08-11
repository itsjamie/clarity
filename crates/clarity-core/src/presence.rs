//! Friend presence: who among a person's mutually-added friends is online and
//! what they are sharing.
//!
//! A connection authenticates as an Ed25519 identity (see [`verify_identity`])
//! and subscribes to a set of friend codes — its contacts. The server reveals a
//! friend's presence to a subscriber only when the interest is **mutual**: both
//! must have added each other. This is what makes "trade codes once and you'll
//! see each other" hold, and it stops a one-sided subscriber from watching
//! someone who never added them back.
//!
//! Concurrency mirrors the room actor: a single task owns [`PresenceState`] and
//! processes [`PresenceCommand`]s serially, so the visibility bookkeeping needs
//! no locks. [`PresenceRegistry`] is the cheap, cloneable handle to it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use clarity_protocol::{
    ErrorCode, FriendPresence, HostedRoom, PROTOCOL_VERSION, PresenceServerMessage, SharingState,
};
use rand::RngCore;
use time::OffsetDateTime;
use tokio::sync::mpsc;

use crate::Clock;
use crate::clock::format_time;

/// Identifies one presence connection. A single identity may hold several (one
/// per device); it counts as online while any remain.
pub type SessionId = u64;

/// The outbound half of one presence connection.
pub type PresenceHandle = mpsc::Sender<PresenceServerMessage>;

#[derive(Debug, thiserror::Error)]
pub enum PresenceAuthError {
    #[error("the presence credentials were malformed")]
    Malformed,
    #[error("the presence signature did not verify")]
    BadSignature,
}

impl PresenceAuthError {
    pub fn code(&self) -> ErrorCode {
        ErrorCode::AuthenticationFailed
    }
}

/// A random challenge for a connecting client to sign, proving it holds the
/// private key behind the public key (and therefore the friend code) it claims.
pub fn new_challenge() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    BASE64.encode(bytes)
}

/// Verifies a signature over `message` and returns the friend code derived
/// from the public key. The derivation — not the client — decides the code, so
/// a client cannot claim a code that is not its own.
pub fn verify_identity(
    public_key_b64: &str,
    signature_b64: &str,
    message: &str,
) -> Result<String, PresenceAuthError> {
    let public_key = BASE64
        .decode(public_key_b64)
        .map_err(|_| PresenceAuthError::Malformed)?;
    let signature = BASE64
        .decode(signature_b64)
        .map_err(|_| PresenceAuthError::Malformed)?;
    if public_key.len() != 32 {
        return Err(PresenceAuthError::Malformed);
    }
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &public_key)
        .verify(message.as_bytes(), &signature)
        .map_err(|_| PresenceAuthError::BadSignature)?;
    Ok(clarity_protocol::code::encode(&public_key))
}

/// Verifies a domain-separated identity signature (see
/// [`clarity_protocol::identity_challenge_payload`]) against each host the
/// server answers to, returning the derived friend code on the first match.
/// The host set is the server's public base URL plus its allowed origins, so
/// a signature bound to another server's host never verifies here.
pub fn verify_identity_for_hosts(
    public_key_b64: &str,
    signature_b64: &str,
    context: &str,
    hosts: &[String],
    nonce: &str,
) -> Result<String, PresenceAuthError> {
    let mut outcome = Err(PresenceAuthError::BadSignature);
    for host in hosts {
        let payload = clarity_protocol::identity_challenge_payload(context, host, nonce);
        match verify_identity(public_key_b64, signature_b64, &payload) {
            Ok(code) => return Ok(code),
            Err(error @ PresenceAuthError::Malformed) => return Err(error),
            Err(error) => outcome = Err(error),
        }
    }
    outcome
}

struct Session {
    code: String,
    outbound: PresenceHandle,
    subscriptions: HashSet<String>,
    hosting: Option<HostedRoom>,
}

/// The presence graph: every live session, indexed only by session id. Codes,
/// online-ness, and hosting are derived from the sessions on demand — a code is
/// online exactly while it has a session, which keeps multi-device correct
/// without extra bookkeeping. `last_seen` remembers when an identity's final
/// session dropped; it is in-memory only and lost on restart.
#[derive(Default)]
pub struct PresenceState {
    sessions: HashMap<SessionId, Session>,
    last_seen: HashMap<String, OffsetDateTime>,
}

impl PresenceState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a newly-authenticated connection. A fresh session has no
    /// subscriptions, so it reveals nothing and is revealed to no one until it
    /// subscribes; registration alone therefore notifies nobody.
    pub fn connect(&mut self, id: SessionId, code: String, outbound: PresenceHandle) {
        self.last_seen.remove(&code);
        self.sessions.insert(
            id,
            Session {
                code,
                outbound,
                subscriptions: HashSet::new(),
                hosting: None,
            },
        );
    }

    /// Replaces a session's watch set. Sends the subscriber a snapshot of the
    /// friends now visible to it, and tells each friend whose mutual status with
    /// this identity changed.
    pub fn subscribe(&mut self, id: SessionId, codes: Vec<String>, now: OffsetDateTime) {
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let code = session.code.clone();
        let previously_visible = self.watchers_of(&code);
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        session.subscriptions = codes.into_iter().collect();
        let now_visible = self.watchers_of(&code);

        let friends: Vec<FriendPresence> = now_visible
            .iter()
            .map(|peer| self.presence_of(peer, now))
            .collect();
        self.send_to_code(&code, snapshot(friends, now));

        // Peers whose view of this identity changed: newly-mutual peers learn it
        // is here; no-longer-mutual peers see it drop away.
        for peer in now_visible.union(&previously_visible) {
            let presence = if self.mutual(&code, peer) {
                self.presence_of(&code, now)
            } else {
                self.offline_presence(&code, now)
            };
            self.send_to_code(peer, update(presence, now));
        }
    }

    /// Updates what this identity is sharing and tells its visible friends.
    pub fn announce(&mut self, id: SessionId, hosting: Option<HostedRoom>, now: OffsetDateTime) {
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        session.hosting = hosting;
        let code = session.code.clone();
        self.notify_watchers(&code, now);
    }

    /// Drops a connection. If it was the identity's last, its friends see it go
    /// offline; otherwise they see its possibly-changed presence.
    pub fn disconnect(&mut self, id: SessionId, now: OffsetDateTime) {
        let Some(session) = self.sessions.remove(&id) else {
            return;
        };
        let code = session.code;
        if !self.online(&code) {
            self.last_seen.insert(code.clone(), now);
        }
        for peer in self.watchers_including(&code, &session.subscriptions) {
            let presence = if self.mutual(&code, &peer) {
                self.presence_of(&code, now)
            } else {
                self.offline_presence(&code, now)
            };
            self.send_to_code(&peer, update(presence, now));
        }
    }

    /// Applies an authoritative viewer-count and sharing-state change to every
    /// session hosting `room_id`, and tells the hosts' visible friends.
    pub fn room_updated(
        &mut self,
        room_id: &str,
        viewer_count: u32,
        sharing_state: SharingState,
        now: OffsetDateTime,
    ) {
        let mut hosts = HashSet::new();
        for session in self.sessions.values_mut() {
            if let Some(hosting) = session
                .hosting
                .as_mut()
                .filter(|hosting| hosting.room_id == room_id)
            {
                hosting.viewer_count = viewer_count;
                hosting.sharing_state = sharing_state;
                hosts.insert(session.code.clone());
            }
        }
        for code in hosts {
            self.notify_watchers(&code, now);
        }
    }

    /// Clears the hosting state of every session hosting `room_id` (the room
    /// closed or expired) and tells the hosts' visible friends.
    pub fn room_closed(&mut self, room_id: &str, now: OffsetDateTime) {
        let mut hosts = HashSet::new();
        for session in self.sessions.values_mut() {
            if session
                .hosting
                .as_ref()
                .is_some_and(|hosting| hosting.room_id == room_id)
            {
                session.hosting = None;
                hosts.insert(session.code.clone());
            }
        }
        for code in hosts {
            self.notify_watchers(&code, now);
        }
    }

    fn notify_watchers(&self, code: &str, now: OffsetDateTime) {
        let presence = self.presence_of(code, now);
        for peer in self.watchers_of(code) {
            self.send_to_code(&peer, update(presence.clone(), now));
        }
    }

    /// Codes that can currently see `code` — those mutually subscribed with it.
    fn watchers_of(&self, code: &str) -> HashSet<String> {
        self.codes()
            .into_iter()
            .filter(|peer| peer != code && self.mutual(code, peer))
            .collect()
    }

    /// Watchers of `code`, plus peers that a now-removed session's subscriptions
    /// meant could see it — so a disconnect still reaches a peer this identity
    /// just stopped being mutual with.
    fn watchers_including(&self, code: &str, removed_subs: &HashSet<String>) -> HashSet<String> {
        let mut watchers = self.watchers_of(code);
        for peer in removed_subs {
            if peer != code && self.code_subscribes(peer, code) {
                watchers.insert(peer.clone());
            }
        }
        watchers
    }

    fn mutual(&self, a: &str, b: &str) -> bool {
        self.code_subscribes(a, b) && self.code_subscribes(b, a)
    }

    fn code_subscribes(&self, code: &str, target: &str) -> bool {
        self.sessions
            .values()
            .any(|session| session.code == code && session.subscriptions.contains(target))
    }

    fn online(&self, code: &str) -> bool {
        self.sessions.values().any(|session| session.code == code)
    }

    fn hosting_of(&self, code: &str) -> Option<HostedRoom> {
        self.sessions
            .values()
            .find(|session| session.code == code && session.hosting.is_some())
            .and_then(|session| session.hosting.clone())
    }

    fn presence_of(&self, code: &str, now: OffsetDateTime) -> FriendPresence {
        FriendPresence {
            code: code.to_owned(),
            online: self.online(code),
            hosting: self.hosting_of(code),
            last_seen_seconds_ago: self.last_seen_seconds_ago(code, now),
        }
    }

    fn offline_presence(&self, code: &str, now: OffsetDateTime) -> FriendPresence {
        FriendPresence {
            code: code.to_owned(),
            online: false,
            hosting: None,
            last_seen_seconds_ago: self.last_seen_seconds_ago(code, now),
        }
    }

    /// Seconds since the identity's last session dropped; `None` while online
    /// or when it has not been seen since the server started.
    fn last_seen_seconds_ago(&self, code: &str, now: OffsetDateTime) -> Option<u64> {
        if self.online(code) {
            return None;
        }
        self.last_seen
            .get(code)
            .map(|at| u64::try_from((now - *at).whole_seconds()).unwrap_or(0))
    }

    fn codes(&self) -> HashSet<String> {
        self.sessions
            .values()
            .map(|session| session.code.clone())
            .collect()
    }

    fn send_to_code(&self, code: &str, message: PresenceServerMessage) {
        for session in self.sessions.values().filter(|s| s.code == code) {
            let _ = session.outbound.try_send(message.clone());
        }
    }
}

fn snapshot(friends: Vec<FriendPresence>, now: OffsetDateTime) -> PresenceServerMessage {
    PresenceServerMessage::Snapshot {
        protocol_version: PROTOCOL_VERSION,
        server_timestamp: format_time(now),
        friends,
    }
}

fn update(friend: FriendPresence, now: OffsetDateTime) -> PresenceServerMessage {
    PresenceServerMessage::Update {
        protocol_version: PROTOCOL_VERSION,
        server_timestamp: format_time(now),
        friend,
    }
}

/// A command to the presence actor.
enum PresenceCommand {
    Connect {
        id: SessionId,
        code: String,
        outbound: PresenceHandle,
    },
    Subscribe {
        id: SessionId,
        codes: Vec<String>,
    },
    Announce {
        id: SessionId,
        hosting: Option<HostedRoom>,
    },
    RoomUpdated {
        room_id: String,
        viewer_count: u32,
        sharing_state: SharingState,
    },
    RoomClosed {
        room_id: String,
    },
    Disconnect {
        id: SessionId,
    },
    Shutdown,
}

/// A cheap, cloneable handle to the presence actor.
#[derive(Clone)]
pub struct PresenceRegistry {
    commands: mpsc::Sender<PresenceCommand>,
    next_id: Arc<AtomicU64>,
}

impl PresenceRegistry {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let (commands, receiver) = mpsc::channel(256);
        tokio::spawn(run_presence_actor(receiver, clock));
        Self {
            commands,
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Registers a verified connection and returns its session id. The caller
    /// pairs this with [`disconnect`](Self::disconnect) on teardown.
    pub fn connect(&self, code: String, outbound: PresenceHandle) -> SessionId {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.commands.try_send(PresenceCommand::Connect {
            id,
            code,
            outbound,
        });
        id
    }

    pub fn subscribe(&self, id: SessionId, codes: Vec<String>) {
        let _ = self.commands.try_send(PresenceCommand::Subscribe { id, codes });
    }

    pub fn announce(&self, id: SessionId, hosting: Option<HostedRoom>) {
        let _ = self
            .commands
            .try_send(PresenceCommand::Announce { id, hosting });
    }

    /// Pushes an authoritative room update to every host of `room_id`.
    pub fn room_updated(&self, room_id: String, viewer_count: u32, sharing_state: SharingState) {
        let _ = self.commands.try_send(PresenceCommand::RoomUpdated {
            room_id,
            viewer_count,
            sharing_state,
        });
    }

    /// Clears the hosting state of every host of `room_id`.
    pub fn room_closed(&self, room_id: String) {
        let _ = self
            .commands
            .try_send(PresenceCommand::RoomClosed { room_id });
    }

    pub fn disconnect(&self, id: SessionId) {
        let _ = self.commands.try_send(PresenceCommand::Disconnect { id });
    }

    pub async fn shutdown(&self) {
        let _ = self.commands.send(PresenceCommand::Shutdown).await;
    }
}

async fn run_presence_actor(mut commands: mpsc::Receiver<PresenceCommand>, clock: Arc<dyn Clock>) {
    let mut state = PresenceState::new();
    while let Some(command) = commands.recv().await {
        let now = clock.now();
        match command {
            PresenceCommand::Connect {
                id,
                code,
                outbound,
            } => state.connect(id, code, outbound),
            PresenceCommand::Subscribe { id, codes } => state.subscribe(id, codes, now),
            PresenceCommand::Announce { id, hosting } => state.announce(id, hosting, now),
            PresenceCommand::RoomUpdated {
                room_id,
                viewer_count,
                sharing_state,
            } => state.room_updated(&room_id, viewer_count, sharing_state, now),
            PresenceCommand::RoomClosed { room_id } => state.room_closed(&room_id, now),
            PresenceCommand::Disconnect { id } => state.disconnect(id, now),
            PresenceCommand::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("valid time")
    }

    fn session() -> (PresenceHandle, mpsc::Receiver<PresenceServerMessage>) {
        mpsc::channel(16)
    }

    /// Drains a receiver into the friend-presences it was told about, last write
    /// per code winning — the net view the connection would hold.
    fn view(rx: &mut mpsc::Receiver<PresenceServerMessage>) -> HashMap<String, FriendPresence> {
        let mut map = HashMap::new();
        while let Ok(message) = rx.try_recv() {
            match message {
                PresenceServerMessage::Snapshot { friends, .. } => {
                    for friend in friends {
                        map.insert(friend.code.clone(), friend);
                    }
                }
                PresenceServerMessage::Update { friend, .. } => {
                    map.insert(friend.code.clone(), friend);
                }
                _ => {}
            }
        }
        map
    }

    const A: &str = "clr-AAAA-AAAA";
    const B: &str = "clr-BBBB-BBBB";

    #[test]
    fn mutual_friends_see_each_other_online() {
        let mut state = PresenceState::new();
        let (a_tx, mut a_rx) = session();
        let (b_tx, mut b_rx) = session();
        state.connect(1, A.to_owned(), a_tx);
        state.connect(2, B.to_owned(), b_tx);

        // A adds B first: not yet mutual, so A sees nobody.
        state.subscribe(1, vec![B.to_owned()], at());
        assert!(view(&mut a_rx).is_empty());
        assert!(view(&mut b_rx).is_empty());

        // B adds A back: now mutual. Each sees the other online.
        state.subscribe(2, vec![A.to_owned()], at());
        let a_view = view(&mut a_rx);
        assert_eq!(a_view.get(B).map(|p| p.online), Some(true));
        let b_view = view(&mut b_rx);
        assert_eq!(b_view.get(A).map(|p| p.online), Some(true));
    }

    #[test]
    fn one_sided_subscription_reveals_nothing() {
        let mut state = PresenceState::new();
        let (a_tx, mut a_rx) = session();
        let (b_tx, mut b_rx) = session();
        state.connect(1, A.to_owned(), a_tx);
        state.connect(2, B.to_owned(), b_tx);

        // A watches B, but B never adds A.
        state.subscribe(1, vec![B.to_owned()], at());
        assert!(view(&mut a_rx).is_empty(), "A must not see an unrequited add");
        assert!(view(&mut b_rx).is_empty(), "B is not told it is being watched");
    }

    #[test]
    fn hosting_and_disconnect_propagate_to_mutual_friends() {
        let mut state = PresenceState::new();
        let (a_tx, _a_rx) = session();
        let (b_tx, mut b_rx) = session();
        state.connect(1, A.to_owned(), a_tx);
        state.connect(2, B.to_owned(), b_tx);
        state.subscribe(1, vec![B.to_owned()], at());
        state.subscribe(2, vec![A.to_owned()], at());
        let _ = view(&mut b_rx); // drain the initial online update

        // A starts sharing: B sees the hosted room.
        let room = HostedRoom {
            room_id: "room1".to_owned(),
            viewer_url: "https://host/r/room1#secret".to_owned(),
            viewer_count: 3,
            sharing_state: SharingState::Live,
        };
        state.announce(1, Some(room.clone()), at());
        assert_eq!(view(&mut b_rx).get(A).and_then(|p| p.hosting.clone()), Some(room));

        // The room actor reports a change: B sees the authoritative count and
        // sharing state without A re-announcing.
        state.room_updated("room1", 5, SharingState::Paused, at());
        let updated = view(&mut b_rx);
        let hosting = updated.get(A).and_then(|p| p.hosting.clone()).expect("hosting");
        assert_eq!(hosting.viewer_count, 5);
        assert_eq!(hosting.sharing_state, SharingState::Paused);

        // The room closes: B sees A stop hosting.
        state.room_closed("room1", at());
        let closed = view(&mut b_rx);
        assert!(closed.get(A).is_some_and(|p| p.hosting.is_none() && p.online));

        // A disconnects: B sees it go offline with a last-seen time.
        state.disconnect(1, at() + time::Duration::seconds(30));
        let gone = view(&mut b_rx);
        assert_eq!(gone.get(A).map(|p| p.online), Some(false));
        // The disconnect update is stamped at the moment it happened.
        assert_eq!(gone.get(A).and_then(|p| p.last_seen_seconds_ago), Some(0));
    }

    #[test]
    fn verify_identity_round_trips_a_real_signature() {
        // Build a keypair with ring and sign a challenge, then verify.
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("keygen");
        let key_pair =
            ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("from pkcs8");
        use ring::signature::KeyPair;
        let public_key = BASE64.encode(key_pair.public_key().as_ref());
        let challenge = new_challenge();
        let signature = BASE64.encode(key_pair.sign(challenge.as_bytes()).as_ref());

        let code = verify_identity(&public_key, &signature, &challenge).expect("verifies");
        assert!(code.starts_with("clr-"));
        // A signature over a different challenge must fail.
        assert!(verify_identity(&public_key, &signature, "other").is_err());
    }

    #[test]
    fn host_bound_signatures_verify_only_for_their_host_and_context() {
        use clarity_protocol::{
            IDENTITY_CONTEXT_PRESENCE, IDENTITY_CONTEXT_ROOM_AUTH, identity_challenge_payload,
        };

        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("keygen");
        let key_pair =
            ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("from pkcs8");
        use ring::signature::KeyPair;
        let public_key = BASE64.encode(key_pair.public_key().as_ref());
        let nonce = new_challenge();
        let payload =
            identity_challenge_payload(IDENTITY_CONTEXT_PRESENCE, "example.test:3000", &nonce);
        let signature = BASE64.encode(key_pair.sign(payload.as_bytes()).as_ref());

        let hosts = vec!["other.test".to_owned(), "example.test:3000".to_owned()];
        let code = verify_identity_for_hosts(
            &public_key,
            &signature,
            IDENTITY_CONTEXT_PRESENCE,
            &hosts,
            &nonce,
        )
        .expect("verifies against its own host");
        assert!(code.starts_with("clr-"));

        // The same signature bound to a host the server does not answer to,
        // or presented in the other context, must fail.
        assert!(
            verify_identity_for_hosts(
                &public_key,
                &signature,
                IDENTITY_CONTEXT_PRESENCE,
                &["evil.test".to_owned()],
                &nonce,
            )
            .is_err()
        );
        assert!(
            verify_identity_for_hosts(
                &public_key,
                &signature,
                IDENTITY_CONTEXT_ROOM_AUTH,
                &hosts,
                &nonce,
            )
            .is_err()
        );
    }
}
