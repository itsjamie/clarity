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

/// How long an unaccepted friend request stands. Adding a friend is something
/// both people do in the moment; an invite the other side has not answered
/// within this window ages out instead of waiting forever. The clients drop
/// their pending contact on the same clock, so the two sides agree to within
/// the sweep interval. Mutual interest — an accepted request — never expires.
pub const REQUEST_TTL: time::Duration = time::Duration::minutes(10);

/// The outbound half of one presence connection.
pub type PresenceHandle = mpsc::Sender<PresenceServerMessage>;

#[derive(Debug, thiserror::Error)]
pub enum PresenceAuthError {
    #[error("the presence credentials were malformed")]
    Malformed,
    #[error("the presence signature did not verify")]
    BadSignature,
}

/// The presence actor has stopped and can no longer accept lifecycle changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("presence service unavailable")]
pub struct PresenceUnavailable;

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
///
/// `standing` records standing interest: requester code → the codes it
/// subscribes to, each with the time the interest was first asserted. Unlike
/// a session's subscriptions it survives disconnects, so a friend request (a
/// one-sided entry) can still be shown when the target connects after the
/// requester left. Each subscribe replaces the identity's whole entry with
/// the union of its live sessions' sets, so a contact removed while offline
/// is withdrawn on the next connect — a session-diff would let an entry from
/// a dead session linger forever. One-sided entries age out after
/// [`REQUEST_TTL`]; mutual ones persist, which also means dropping an old
/// friend never resurfaces their long-expired interest as a fresh invite.
/// Like `last_seen` it is lost on restart, but every client resubscribes its
/// full contact set on connect, so it self-heals.
#[derive(Default)]
pub struct PresenceState {
    sessions: HashMap<SessionId, Session>,
    last_seen: HashMap<String, OffsetDateTime>,
    standing: HashMap<String, HashMap<String, OffsetDateTime>>,
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
    /// this identity changed. Also maintains the standing `wants` interest and
    /// keeps everyone's pending-request view current: the subscriber always gets
    /// its own `presence:requests` set, and each target this identity just added
    /// or withdrew from gets its recomputed set.
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

        // Standing interest becomes the union of this identity's LIVE
        // sessions' sets, not a diff against this session's previous set: a
        // diff can never withdraw an entry a dead session registered, so a
        // contact removed while offline would leave the target a phantom
        // request until the server restarted. The union keeps multi-device
        // correct (a device dropping a contact another live device still has
        // withdraws nothing), with one accepted wrinkle: a device
        // resubscribing while its sibling is offline narrows the identity's
        // standing set to what the live devices hold, until the sibling
        // reconnects and re-registers its own contacts.
        let union: HashSet<String> = self
            .sessions
            .values()
            .filter(|session| session.code == code)
            .flat_map(|session| session.subscriptions.iter().cloned())
            .collect();
        let mut old_standing = self.standing.remove(&code).unwrap_or_default();
        let added: Vec<String> = union
            .iter()
            .filter(|target| !old_standing.contains_key(*target))
            .cloned()
            .collect();
        let removed: Vec<String> = old_standing
            .keys()
            .filter(|target| !union.contains(*target))
            .cloned()
            .collect();
        if !union.is_empty() {
            // A re-asserted target keeps its original timestamp, so a client
            // reconnecting cannot keep an unanswered invite alive past the
            // TTL; only a genuinely new add starts the clock.
            let fresh: HashMap<String, OffsetDateTime> = union
                .into_iter()
                .map(|target| {
                    let asserted = old_standing.remove(&target).unwrap_or(now);
                    (target, asserted)
                })
                .collect();
            self.standing.insert(code.clone(), fresh);
        }

        let friends: Vec<FriendPresence> = now_visible
            .iter()
            .map(|peer| self.presence_of(peer, now))
            .collect();
        self.send_to_code(&code, snapshot(friends, now));
        self.send_to_code(&code, requests(self.requests_of(&code, now), now));

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

        // Targets this identity just added or withdrew from: their pending set
        // may have gained or lost this code, so push the recomputed set.
        for target in added.iter().chain(&removed) {
            if target != &code {
                self.send_to_code(target, requests(self.requests_of(target, now), now));
            }
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

    /// The codes wanting `code` without reciprocation and within
    /// [`REQUEST_TTL`] — its pending friend requests. Judged on `standing`
    /// (not live sessions), so a request stands while the requester is
    /// offline and clears the moment `code` subscribes back, online or not.
    /// The scan over all standing entries is proportional to the identities
    /// with contacts, which the subscription cap keeps bounded. Sorted, so
    /// pushes are deterministic.
    fn requests_of(&self, code: &str, now: OffsetDateTime) -> Vec<String> {
        let reciprocated = self.standing.get(code);
        let mut codes: Vec<String> = self
            .standing
            .iter()
            .filter(|(requester, targets)| {
                requester.as_str() != code
                    && targets
                        .get(code)
                        .is_some_and(|asserted| now - *asserted < REQUEST_TTL)
                    && !reciprocated.is_some_and(|wanted| wanted.contains_key(requester.as_str()))
            })
            .map(|(requester, _)| requester.clone())
            .collect();
        codes.sort();
        codes
    }

    /// Drops one-sided standing entries older than [`REQUEST_TTL`] and pushes
    /// the emptied request sets to affected targets that are online, so an
    /// invite whose sender never returned still disappears on time. Mutual
    /// entries are a friendship, not a request, and are left alone. Driven by
    /// the registry's periodic sweep; `requests_of` filters by age anyway, so
    /// the sweep only affects when the withdrawal becomes visible and when
    /// the memory is reclaimed.
    pub fn expire_requests(&mut self, now: OffsetDateTime) {
        let expired: Vec<(String, String)> = self
            .standing
            .iter()
            .flat_map(|(requester, targets)| {
                targets
                    .iter()
                    .filter(|(target, asserted)| {
                        let mutual = self
                            .standing
                            .get(*target)
                            .is_some_and(|wanted| wanted.contains_key(requester.as_str()));
                        !mutual && now - **asserted >= REQUEST_TTL
                    })
                    .map(|(target, _)| (requester.clone(), target.clone()))
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut affected: HashSet<String> = HashSet::new();
        for (requester, target) in expired {
            if let Some(targets) = self.standing.get_mut(&requester) {
                targets.remove(&target);
                if targets.is_empty() {
                    self.standing.remove(&requester);
                }
            }
            affected.insert(target);
        }
        for target in affected {
            self.send_to_code(&target, requests(self.requests_of(&target, now), now));
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

fn requests(codes: Vec<String>, now: OffsetDateTime) -> PresenceServerMessage {
    PresenceServerMessage::Requests {
        protocol_version: PROTOCOL_VERSION,
        server_timestamp: format_time(now),
        codes,
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
    /// The periodic request sweep; see [`PresenceState::expire_requests`].
    Expire,
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
        // The request sweep: much finer than the TTL it enforces, so an
        // invite disappears close to on time. Ends itself when the actor
        // drops its receiver.
        let sweep = commands.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tick.tick().await;
                if sweep.send(PresenceCommand::Expire).await.is_err() {
                    break;
                }
            }
        });
        Self {
            commands,
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Registers a verified connection and returns its session id. The caller
    /// pairs this with [`disconnect`](Self::disconnect) on teardown.
    pub async fn connect(
        &self,
        code: String,
        outbound: PresenceHandle,
    ) -> Result<SessionId, PresenceUnavailable> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.commands
            .send(PresenceCommand::Connect {
                id,
                code,
                outbound,
            })
            .await
            .map_err(|_| PresenceUnavailable)?;
        Ok(id)
    }

    pub async fn subscribe(
        &self,
        id: SessionId,
        codes: Vec<String>,
    ) -> Result<(), PresenceUnavailable> {
        self.commands
            .send(PresenceCommand::Subscribe { id, codes })
            .await
            .map_err(|_| PresenceUnavailable)
    }

    pub async fn announce(
        &self,
        id: SessionId,
        hosting: Option<HostedRoom>,
    ) -> Result<(), PresenceUnavailable> {
        self.commands
            .send(PresenceCommand::Announce { id, hosting })
            .await
            .map_err(|_| PresenceUnavailable)
    }

    /// Pushes an authoritative room update to every host of `room_id`.
    pub async fn room_updated(
        &self,
        room_id: String,
        viewer_count: u32,
        sharing_state: SharingState,
    ) -> Result<(), PresenceUnavailable> {
        self.commands
            .send(PresenceCommand::RoomUpdated {
                room_id,
                viewer_count,
                sharing_state,
            })
            .await
            .map_err(|_| PresenceUnavailable)
    }

    /// Clears the hosting state of every host of `room_id`.
    pub async fn room_closed(&self, room_id: String) -> Result<(), PresenceUnavailable> {
        self.commands
            .send(PresenceCommand::RoomClosed { room_id })
            .await
            .map_err(|_| PresenceUnavailable)
    }

    pub async fn disconnect(&self, id: SessionId) -> Result<(), PresenceUnavailable> {
        self.commands
            .send(PresenceCommand::Disconnect { id })
            .await
            .map_err(|_| PresenceUnavailable)
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
            PresenceCommand::Expire => state.expire_requests(now),
            PresenceCommand::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_waits_for_capacity_instead_of_dropping_lifecycle_commands() {
        let (commands, mut receiver) = mpsc::channel(1);
        let registry = PresenceRegistry {
            commands: commands.clone(),
            next_id: Arc::new(AtomicU64::new(1)),
        };
        commands
            .send(PresenceCommand::Expire)
            .await
            .expect("fill actor queue");
        let (outbound, _outbound_rx) = session();
        let connecting = tokio::spawn({
            let registry = registry.clone();
            async move { registry.connect("clr-AAAA-AAAA".to_owned(), outbound).await }
        });

        tokio::task::yield_now().await;
        assert!(!connecting.is_finished(), "connect was dropped instead of waiting");
        assert!(matches!(receiver.recv().await, Some(PresenceCommand::Expire)));
        assert_eq!(connecting.await.expect("connect task"), Ok(1));
        assert!(matches!(
            receiver.recv().await,
            Some(PresenceCommand::Connect { id: 1, .. })
        ));

        drop(receiver);
        assert_eq!(
            registry.subscribe(1, Vec::new()).await,
            Err(PresenceUnavailable)
        );
    }

    fn at() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("valid time")
    }

    /// A moment past the request TTL, for aging invites out.
    fn past_ttl() -> OffsetDateTime {
        at() + REQUEST_TTL + time::Duration::seconds(1)
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

    /// Drains a receiver into the last pending-request set it was told, `None`
    /// when no requests message arrived. Other message kinds are discarded.
    fn last_requests(rx: &mut mpsc::Receiver<PresenceServerMessage>) -> Option<Vec<String>> {
        let mut last = None;
        while let Ok(message) = rx.try_recv() {
            if let PresenceServerMessage::Requests { codes, .. } = message {
                last = Some(codes);
            }
        }
        last
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
    fn one_sided_subscription_reveals_a_request_but_no_presence() {
        let mut state = PresenceState::new();
        let (a_tx, mut a_rx) = session();
        let (b_tx, mut b_rx) = session();
        state.connect(1, A.to_owned(), a_tx);
        state.connect(2, B.to_owned(), b_tx);

        // A watches B, but B never adds A.
        state.subscribe(1, vec![B.to_owned()], at());
        assert!(view(&mut a_rx).is_empty(), "A must not see an unrequited add");
        // B is told who is waiting — that is the friend request — but learns
        // nothing about A's presence until the pair is mutual.
        let mut b_presence = 0;
        let mut b_requests = None;
        while let Ok(message) = b_rx.try_recv() {
            match message {
                PresenceServerMessage::Snapshot { friends, .. } => b_presence += friends.len(),
                PresenceServerMessage::Update { .. } => b_presence += 1,
                PresenceServerMessage::Requests { codes, .. } => b_requests = Some(codes),
                _ => {}
            }
        }
        assert_eq!(b_presence, 0, "B sees no presence from an unrequited add");
        assert_eq!(b_requests, Some(vec![A.to_owned()]));
    }

    #[test]
    fn a_request_waits_for_a_target_that_subscribes_later() {
        let mut state = PresenceState::new();
        let (a_tx, _a_rx) = session();
        state.connect(1, A.to_owned(), a_tx);
        state.subscribe(1, vec![B.to_owned()], at());
        // A leaves; the standing interest survives the disconnect.
        state.disconnect(1, at());

        let (b_tx, mut b_rx) = session();
        state.connect(2, B.to_owned(), b_tx);
        state.subscribe(2, Vec::new(), at());
        assert_eq!(last_requests(&mut b_rx), Some(vec![A.to_owned()]));
    }

    #[test]
    fn adding_back_clears_the_request_on_both_sides() {
        let mut state = PresenceState::new();
        let (a_tx, mut a_rx) = session();
        let (b_tx, mut b_rx) = session();
        state.connect(1, A.to_owned(), a_tx);
        state.connect(2, B.to_owned(), b_tx);

        state.subscribe(1, vec![B.to_owned()], at());
        assert_eq!(last_requests(&mut b_rx), Some(vec![A.to_owned()]));

        // B accepts by adding A: the pair is mutual, so neither side has a
        // pending request any more.
        state.subscribe(2, vec![A.to_owned()], at());
        assert_eq!(last_requests(&mut b_rx), Some(Vec::new()));
        assert_eq!(last_requests(&mut a_rx), Some(Vec::new()));
    }

    #[test]
    fn withdrawing_the_subscription_withdraws_the_request() {
        let mut state = PresenceState::new();
        let (a_tx, _a_rx) = session();
        let (b_tx, mut b_rx) = session();
        state.connect(1, A.to_owned(), a_tx);
        state.connect(2, B.to_owned(), b_tx);

        state.subscribe(1, vec![B.to_owned()], at());
        assert_eq!(last_requests(&mut b_rx), Some(vec![A.to_owned()]));

        // A cancels the invite by resubscribing without B.
        state.subscribe(1, Vec::new(), at());
        assert_eq!(last_requests(&mut b_rx), Some(Vec::new()));
    }

    /// An invite is something done in the moment: unanswered past the TTL,
    /// it no longer greets a target who connects later.
    #[test]
    fn an_unanswered_request_ages_out() {
        let mut state = PresenceState::new();
        let (a_tx, _a_rx) = session();
        state.connect(1, A.to_owned(), a_tx);
        state.subscribe(1, vec![B.to_owned()], at());
        state.disconnect(1, at());

        let (b_tx, mut b_rx) = session();
        state.connect(2, B.to_owned(), b_tx);
        state.subscribe(2, Vec::new(), past_ttl());
        assert_eq!(last_requests(&mut b_rx), Some(Vec::new()));
    }

    /// The sweep pushes the withdrawal to a target already watching, so the
    /// invite disappears on time even though the sender never came back.
    #[test]
    fn the_sweep_withdraws_an_aged_request_live() {
        let mut state = PresenceState::new();
        let (a_tx, _a_rx) = session();
        let (b_tx, mut b_rx) = session();
        state.connect(1, A.to_owned(), a_tx);
        state.connect(2, B.to_owned(), b_tx);
        state.subscribe(1, vec![B.to_owned()], at());
        state.subscribe(2, Vec::new(), at());
        state.disconnect(1, at());
        assert_eq!(last_requests(&mut b_rx), Some(vec![A.to_owned()]));

        state.expire_requests(past_ttl());
        assert_eq!(last_requests(&mut b_rx), Some(Vec::new()));
    }

    /// A reconnect re-asserting the same contact keeps the original clock: a
    /// client cannot keep an unanswered invite alive by resubscribing.
    #[test]
    fn resubscribing_does_not_restart_an_invite_clock() {
        let mut state = PresenceState::new();
        let (a_tx, _a_rx) = session();
        state.connect(1, A.to_owned(), a_tx);
        state.subscribe(1, vec![B.to_owned()], at());
        state.disconnect(1, at());

        let (a_tx, _a_rx) = session();
        state.connect(3, A.to_owned(), a_tx);
        state.subscribe(3, vec![B.to_owned()], past_ttl());

        let (b_tx, mut b_rx) = session();
        state.connect(2, B.to_owned(), b_tx);
        state.subscribe(2, Vec::new(), past_ttl());
        assert_eq!(last_requests(&mut b_rx), Some(Vec::new()));
    }

    /// Mutual interest is a friendship, not a request: the sweep leaves it
    /// alone however old it is, and unfriending later must not resurface the
    /// other side's long-expired interest as a fresh invite.
    #[test]
    fn an_accepted_pair_outlives_the_ttl_and_unfriending_invites_nobody() {
        let mut state = PresenceState::new();
        let (a_tx, mut a_rx) = session();
        let (b_tx, mut b_rx) = session();
        state.connect(1, A.to_owned(), a_tx);
        state.connect(2, B.to_owned(), b_tx);
        state.subscribe(1, vec![B.to_owned()], at());
        state.subscribe(2, vec![A.to_owned()], at());
        state.expire_requests(past_ttl());
        assert_eq!(view(&mut b_rx).get(A).map(|p| p.online), Some(true));

        // A drops B much later; B's interest in A is long past the TTL, so A
        // sees no invite from the friend it just removed.
        state.subscribe(1, Vec::new(), past_ttl());
        assert_eq!(last_requests(&mut a_rx), Some(Vec::new()));
    }

    /// The phantom-invite regression: interest registered by a session that
    /// then disconnects must be withdrawable by a later session of the same
    /// identity. A session-diff can never see the dead session's entries, so
    /// the reconciliation replaces the identity's standing set wholesale.
    #[test]
    fn a_contact_removed_while_offline_withdraws_the_request() {
        let mut state = PresenceState::new();
        let (a_tx, _a_rx) = session();
        state.connect(1, A.to_owned(), a_tx);
        state.subscribe(1, vec![B.to_owned()], at());
        state.disconnect(1, at());

        // A removed B from its contacts while offline, then reconnects and
        // resubscribes without B.
        let (a_tx, _a_rx) = session();
        state.connect(3, A.to_owned(), a_tx);
        state.subscribe(3, Vec::new(), at());

        let (b_tx, mut b_rx) = session();
        state.connect(2, B.to_owned(), b_tx);
        state.subscribe(2, Vec::new(), at());
        assert_eq!(last_requests(&mut b_rx), Some(Vec::new()));
    }

    /// The live variant: B is already connected when A's reconnect withdraws
    /// the dead session's interest, so B must be pushed the emptied set.
    #[test]
    fn a_reconnect_without_the_contact_clears_the_request_live() {
        let mut state = PresenceState::new();
        let (a_tx, _a_rx) = session();
        let (b_tx, mut b_rx) = session();
        state.connect(1, A.to_owned(), a_tx);
        state.connect(2, B.to_owned(), b_tx);
        state.subscribe(1, vec![B.to_owned()], at());
        state.subscribe(2, Vec::new(), at());
        assert_eq!(last_requests(&mut b_rx), Some(vec![A.to_owned()]));

        state.disconnect(1, at());
        let (a_tx, _a_rx) = session();
        state.connect(3, A.to_owned(), a_tx);
        state.subscribe(3, Vec::new(), at());
        assert_eq!(last_requests(&mut b_rx), Some(Vec::new()));
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
