//! The application's view state — which screen is shown, plus the palette and
//! theatre overlays and the live session views.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use clarity_protocol::{FriendPresence, HostedRoom, SharingState};

/// One room chat message. `own` is true for messages this user sent.
#[derive(Clone)]
pub struct ChatMessage {
    pub from: String,
    pub text: String,
    pub own: bool,
}

/// The latest friend presence, as fed from the background presence connection.
/// Render code reads this; the connection (in `crate::presence`) writes it.
#[derive(Default)]
pub struct PresenceView {
    /// Friend code → their presence. Absent means never-seen (treat as offline).
    pub friends: HashMap<String, FriendPresence>,
    /// This identity's own code, echoed by the server after the handshake.
    pub self_code: Option<String>,
    pub connected: bool,
}

impl PresenceView {
    /// Friends currently hosting a room (live or idle), as (code, room).
    pub fn live(&self) -> impl Iterator<Item = (&String, &HostedRoom)> {
        self.friends
            .iter()
            .filter_map(|(code, friend)| friend.hosting.as_ref().map(|room| (code, room)))
    }
}

/// One viewer of the local presenter's room, and their live send-side stats.
#[derive(Default, Clone)]
pub struct ViewerCard {
    pub name: String,
    pub connected: bool,
    pub bitrate_kbps: Option<u32>,
    pub round_trip_ms: Option<f64>,
    pub packets_lost: Option<i64>,
    /// Cumulative packets sent to this viewer, the denominator for loss %.
    pub packets_sent: Option<u64>,
    pub target_kbps: u32,
    /// The codec encoding this viewer's stream ("AV1", "H264", "VP8").
    pub codec: Option<String>,
}

/// A rolling window of bitrate samples for the diagnostics sparkline. Stats
/// arrive roughly once a second; samples closer together than half that
/// overwrite the newest entry instead of piling up (the presenter records once
/// per per-viewer stats event).
#[derive(Default)]
pub struct BitrateHistory {
    samples: VecDeque<(Instant, u32)>,
}

impl BitrateHistory {
    /// The design's sparkline spans the last minute.
    pub const WINDOW: Duration = Duration::from_secs(60);

    pub fn record(&mut self, kbps: u32) {
        self.record_at(Instant::now(), kbps);
    }

    /// Records a sample at an explicit instant (seeding, tests).
    pub fn record_at(&mut self, at: Instant, kbps: u32) {
        match self.samples.back_mut() {
            Some(last) if at.duration_since(last.0) < Duration::from_millis(500) => {
                last.1 = kbps;
            }
            _ => self.samples.push_back((at, kbps)),
        }
        while let Some(front) = self.samples.front() {
            if at.duration_since(front.0) > Self::WINDOW {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn samples(&self) -> &VecDeque<(Instant, u32)> {
        &self.samples
    }
}

/// A viewer waiting at the door of an approval-required room.
#[derive(Clone)]
pub struct JoinRequest {
    pub peer_id: String,
    pub name: Option<String>,
    pub friend_code: Option<String>,
}

/// The room's remaining life: server-clock seconds at the moment they were
/// reported, so the UI can render a live countdown between snapshots.
#[derive(Clone, Copy)]
pub struct RoomCountdown {
    pub seconds: u64,
    pub at: Instant,
}

impl RoomCountdown {
    pub fn remaining_seconds(&self) -> u64 {
        self.seconds.saturating_sub(self.at.elapsed().as_secs())
    }

    /// "3h 12m left" / "12m left", per the design's header meta.
    pub fn label(&self) -> String {
        let minutes = self.remaining_seconds() / 60;
        if minutes >= 60 {
            format!("{}h {:02}m left", minutes / 60, minutes % 60)
        } else {
            format!("{minutes}m left")
        }
    }
}

/// State of the local presenter session, when this app hosts the room.
/// Written from the presenter connection (`crate::presenter`), read by the Room
/// screen.
#[derive(Default)]
pub struct PresenterView {
    /// A session exists (opening, idle, or sharing).
    pub active: bool,
    /// The room was created on the server.
    pub open: bool,
    /// Whether the room is idle, live, or paused. The room outlives sharing:
    /// stopping a share returns to `Idle` without closing anything.
    pub sharing: SharingState,
    /// A short human status line, e.g. "Creating room…" or an error.
    pub status: String,
    /// The session ended (cleanly or with an error); `status` carries the
    /// reason. With `open` false the start itself failed.
    pub ended: bool,
    /// The signaling connection dropped and is being re-established.
    pub reconnecting: bool,
    pub room_id: Option<String>,
    pub viewer_url: Option<String>,
    /// The room's presenter secret, proving hosting announcements to the
    /// presence channel. Never rendered.
    pub presenter_secret: Option<secrecy::SecretString>,
    pub countdown: Option<RoomCountdown>,
    /// A local preview of the captured screen: the newest frame, uploaded as a
    /// texture by `crate::presenter`, and its pixel size.
    pub texture: Option<egui::TextureHandle>,
    pub frame_size: Option<(u32, u32)>,
    pub viewers: HashMap<String, ViewerCard>,
    /// Viewers waiting for approval, oldest first.
    pub requests: Vec<JoinRequest>,
    pub messages: Vec<ChatMessage>,
    /// Total outgoing bitrate over the last minute, for the sparkline.
    pub bitrate_history: BitrateHistory,
    /// The configured encoder ceiling in kbps; a viewer whose `target_kbps`
    /// sits below it has been downshifted by the rate controller.
    pub target_ceiling_kbps: u32,
}

impl PresenterView {
    pub fn connected_viewers(&self) -> usize {
        self.viewers.values().filter(|v| v.connected).count()
    }

    pub fn is_live(&self) -> bool {
        self.sharing == SharingState::Live
    }
}

/// State of a viewing session, when this app is watching a friend's room. The
/// decoded video lands in `texture`; the rest drives the header and diagnostics.
#[derive(Default)]
pub struct ViewerView {
    pub active: bool,
    /// The stream connected at least once (phase reached Live).
    pub live: bool,
    pub status: String,
    /// The room's sharing state, as the server reports it. `None` until the
    /// first snapshot arrives.
    pub sharing: Option<SharingState>,
    /// The peer connection dropped and recovery (automatic ICE restart) is
    /// under way.
    pub reconnecting: bool,
    pub presenter_connected: bool,
    pub room_code: Option<String>,
    pub countdown: Option<RoomCountdown>,
    /// The most recent decoded frame, uploaded as a texture by `crate::viewer`.
    pub texture: Option<egui::TextureHandle>,
    pub frame_size: Option<(u32, u32)>,
    /// Video renders on a native Wayland subsurface below the window instead
    /// of `texture`; the stage stays unpainted so it shows through.
    pub native_video: bool,
    /// Where the stage's video box was laid out last frame, in window points.
    /// The native path cuts its transparent hole and places the subsurface here.
    pub stage_rect: Option<egui::Rect>,
    pub bitrate_kbps: Option<u32>,
    pub round_trip_ms: Option<f64>,
    pub packets_lost: Option<i64>,
    /// Cumulative packets received, the denominator half for loss %.
    pub packets_received: Option<u64>,
    pub fps: Option<f64>,
    pub codec: Option<String>,
    pub messages: Vec<ChatMessage>,
    /// Incoming bitrate over the last minute, for the sparkline.
    pub bitrate_history: BitrateHistory,
}

impl ViewerView {
    /// The presenter's picture should be on screen: the stream has connected
    /// and the room says something is being shared.
    pub fn showing_video(&self) -> bool {
        self.live && self.sharing.unwrap_or(SharingState::Live) == SharingState::Live
    }
}

/// A request from the UI to the viewing session, consumed by the app loop.
pub enum ViewerCommand {
    /// Join the room at this viewer URL.
    Join(String),
    Leave,
    /// Playback volume, `0.0` (muted) to `1.0`.
    SetVolume(f64),
    /// Rename this viewer for the room and for chat.
    SetName(String),
    /// Ask the presenter for an ICE restart now.
    RestartIce,
}

/// A request from the UI to the presenter session, consumed by the app loop
/// (which owns the runtime and the portal picker).
pub enum PresenterCommand {
    /// Create a room and open it idle (the create modal's "Open room").
    OpenRoom,
    /// Run the picker and start sharing into the open room.
    StartShare,
    /// Run the picker again and swap the capture mid-stream.
    ChangeSource,
    PauseShare,
    ResumeShare,
    /// Back to idle: capture ends, room and chat stay up.
    StopShare,
    /// End the room for everyone.
    CloseRoom,
    /// Detach from the room without closing it (it stays resumable), or
    /// dismiss an ended session.
    Leave,
    Approve(String),
    Deny(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Screen {
    Home,
    Room,
    Friends,
    Settings,
    Onboarding,
}

/// The room's right-panel tab, matching the design's Chat / Diagnostics tabs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RoomTab {
    Chat,
    Diagnostics,
}

/// How the stage fits the video: aspect-fit (letterbox), cover (crop to fill),
/// or native 1:1. Drives the stage's Fit / Fill / 1:1 control.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StageFit {
    Fit,
    Fill,
    Native,
}

/// Who may join a new room — the create modal's "Who can join" dropdown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RoomAccess {
    FriendsOnly,
    AnyoneWithLink,
    AskFirst,
}

impl RoomAccess {
    pub const ALL: [RoomAccess; 3] = [
        RoomAccess::FriendsOnly,
        RoomAccess::AnyoneWithLink,
        RoomAccess::AskFirst,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RoomAccess::FriendsOnly => "Friends only",
            RoomAccess::AnyoneWithLink => "Anyone with the link",
            RoomAccess::AskFirst => "Ask me first",
        }
    }
}

/// How long a new room lives — the create modal's "Room expires in" dropdown.
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum RoomExpiry {
    TwoHours,
    FourHours,
    EightHours,
}

impl RoomExpiry {
    pub const ALL: [RoomExpiry; 3] = [
        RoomExpiry::TwoHours,
        RoomExpiry::FourHours,
        RoomExpiry::EightHours,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RoomExpiry::TwoHours => "2 hours",
            RoomExpiry::FourHours => "4 hours",
            RoomExpiry::EightHours => "8 hours",
        }
    }

    pub fn seconds(self) -> u32 {
        match self {
            RoomExpiry::TwoHours => 2 * 60 * 60,
            RoomExpiry::FourHours => 4 * 60 * 60,
            RoomExpiry::EightHours => 8 * 60 * 60,
        }
    }
}

pub struct AppState {
    pub screen: Screen,
    pub palette_open: bool,
    /// The create-room modal, launched from the sidebar, ⌘N, or the palette.
    pub create_open: bool,
    /// The join-by-link modal, launched from the sidebar.
    pub join_open: bool,
    /// The room link typed into the join modal, and the last parse error.
    pub join_draft: String,
    pub join_error: Option<String>,
    /// The room's right-panel tab and the stage's fit mode.
    pub room_tab: RoomTab,
    pub stage_fit: StageFit,
    /// Theatre mode hides the app sidebar and the room's right panel for a
    /// full-width stage.
    pub theatre: bool,
    /// Whether the floating theatre chat window is shown.
    pub theatre_chat_open: bool,
    /// Where the floating chat was last dragged; `None` uses the default spot.
    pub theatre_chat_pos: Option<egui::Pos2>,
    /// Create-room modal form: capture profile (Motion vs Text), who can join,
    /// and how long the room lives, plus the last create validation error.
    pub new_room_motion: bool,
    pub new_room_access: RoomAccess,
    pub new_room_expiry: RoomExpiry,
    pub create_error: Option<String>,
    /// Draft text for the room chat and palette inputs.
    pub chat_draft: String,
    pub palette_query: String,
    /// The viewer control bar's volume state. The effective volume
    /// (`muted` → 0) is sent to the session on change.
    pub viewer_volume: f32,
    pub viewer_muted: bool,
    /// The control bar's inline "Set name" editor.
    pub name_edit_open: bool,
    pub name_edit_draft: String,
    /// Where the last diagnostics export landed (or why it failed), shown
    /// under the export button.
    pub report_note: Option<String>,

    /// Device-local identity, contacts, and settings.
    pub store: clarity_identity::Store,
    /// Editable copies of the identity's names, bound to the Settings and
    /// Onboarding inputs and written back on change.
    pub name_draft: String,
    pub device_draft: String,
    /// The Clarity server URL, bound to the first-run and Settings inputs.
    pub server_draft: String,
    /// The Friends "add by code" inputs.
    pub friend_code_draft: String,
    pub friend_name_draft: String,
    /// Last add-a-friend error, shown until the next attempt.
    pub friend_error: Option<String>,

    /// Live friend presence, written each frame from the presence connection.
    pub presence_view: PresenceView,
    /// Local presenter session state, when this app hosts a room.
    pub presenter_view: PresenterView,
    /// Pending room/sharing requests from buttons, consumed in order by the
    /// app loop (which owns the runtime the presenter session needs).
    pub presenter_commands: Vec<PresenterCommand>,
    /// Live viewing session state, when this app is watching a friend.
    pub viewer_view: ViewerView,
    /// Pending join/leave/volume requests from the viewer UI.
    pub viewer_commands: Vec<ViewerCommand>,
    /// Chat typed this frame, delivered to whichever session is active.
    pub chat_out: Vec<String>,
}

impl AppState {
    /// Builds the initial state from persisted data. First run — no identity —
    /// opens on onboarding; otherwise home.
    pub fn new(store: clarity_identity::Store) -> Self {
        let screen = if store.identity.is_some() {
            Screen::Home
        } else {
            Screen::Onboarding
        };
        let (name_draft, device_draft) = match &store.identity {
            Some(identity) => (
                identity.display_name().to_owned(),
                identity.device_name().to_owned(),
            ),
            None => (String::new(), default_device_name()),
        };
        let server_draft = store.settings.signaling_server.clone();
        Self {
            screen,
            palette_open: false,
            create_open: false,
            join_open: false,
            join_draft: String::new(),
            join_error: None,
            room_tab: RoomTab::Chat,
            stage_fit: StageFit::Fit,
            theatre: false,
            theatre_chat_open: true,
            theatre_chat_pos: None,
            new_room_motion: false,
            new_room_access: RoomAccess::FriendsOnly,
            new_room_expiry: RoomExpiry::TwoHours,
            create_error: None,
            chat_draft: String::new(),
            palette_query: String::new(),
            viewer_volume: 1.0,
            viewer_muted: false,
            name_edit_open: false,
            name_edit_draft: String::new(),
            report_note: None,
            store,
            name_draft,
            device_draft,
            server_draft,
            friend_code_draft: String::new(),
            friend_name_draft: String::new(),
            friend_error: None,
            presence_view: PresenceView::default(),
            presenter_view: PresenterView::default(),
            presenter_commands: Vec::new(),
            viewer_view: ViewerView::default(),
            viewer_commands: Vec::new(),
            chat_out: Vec::new(),
        }
    }

    /// The name chat carries for messages this user sends.
    pub fn own_display_name(&self) -> String {
        self.store
            .identity
            .as_ref()
            .map(|identity| identity.display_name().to_owned())
            .unwrap_or_else(|| "You".to_owned())
    }

    /// Sends a chat message to the active room and echoes it locally. No-op
    /// when there is no live session.
    pub fn send_chat(&mut self, text: String) {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        let message = ChatMessage {
            from: self.own_display_name(),
            text: text.clone(),
            own: true,
        };
        if self.presenter_view.active {
            self.presenter_view.messages.push(message);
        } else if self.viewer_view.active {
            self.viewer_view.messages.push(message);
        } else {
            return;
        }
        self.chat_out.push(text);
    }

    /// Requests joining the room at `viewer_url` and shows the room.
    pub fn join_room(&mut self, viewer_url: String) {
        self.viewer_commands.push(ViewerCommand::Join(viewer_url));
        self.go(Screen::Room);
    }

    /// Requests leaving the current viewing session.
    pub fn leave_room(&mut self) {
        self.viewer_commands.push(ViewerCommand::Leave);
    }

    /// Requests opening an idle room and shows it. The app loop creates the
    /// session; sharing starts later with [`start_share`](Self::start_share).
    pub fn open_room(&mut self) {
        self.presenter_commands.push(PresenterCommand::OpenRoom);
        self.go(Screen::Room);
    }

    pub fn start_share(&mut self) {
        self.presenter_commands.push(PresenterCommand::StartShare);
    }

    pub fn change_source(&mut self) {
        self.presenter_commands.push(PresenterCommand::ChangeSource);
    }

    pub fn pause_share(&mut self) {
        self.presenter_commands.push(PresenterCommand::PauseShare);
    }

    pub fn resume_share(&mut self) {
        self.presenter_commands.push(PresenterCommand::ResumeShare);
    }

    /// Requests the current share to stop, keeping the room open.
    pub fn stop_share(&mut self) {
        self.presenter_commands.push(PresenterCommand::StopShare);
    }

    /// Requests ending the room for everyone.
    pub fn close_room(&mut self) {
        self.presenter_commands.push(PresenterCommand::CloseRoom);
    }

    /// Detaches from the hosted room, leaving it open on the server.
    pub fn leave_presenter(&mut self) {
        self.presenter_commands.push(PresenterCommand::Leave);
    }

    /// Answers a pending join request and removes it from the queue.
    pub fn answer_request(&mut self, peer_id: String, admit: bool) {
        self.presenter_view
            .requests
            .retain(|request| request.peer_id != peer_id);
        self.presenter_commands.push(if admit {
            PresenterCommand::Approve(peer_id)
        } else {
            PresenterCommand::Deny(peer_id)
        });
    }
}

/// The hostname, as a friendly default for a new identity's device name.
fn default_device_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "This device".to_owned())
}

impl AppState {
    /// Theatre only applies inside the room.
    pub fn theatre_active(&self) -> bool {
        self.theatre && self.screen == Screen::Room
    }

    /// The sidebar is present everywhere except theatre mode and the first-run
    /// onboarding screen, which is a full-bleed welcome with no room context.
    pub fn shows_sidebar(&self) -> bool {
        !self.theatre_active() && self.screen != Screen::Onboarding
    }

    pub fn go(&mut self, screen: Screen) {
        self.screen = screen;
        self.palette_open = false;
    }

    /// Opens the command palette with a fresh query.
    pub fn open_palette(&mut self) {
        self.palette_query.clear();
        self.palette_open = true;
    }

    /// Opens the create-room modal over Home (the design's create flow).
    pub fn open_create(&mut self) {
        self.screen = Screen::Home;
        self.palette_open = false;
        self.join_open = false;
        self.create_error = None;
        self.create_open = true;
    }

    /// Opens the join-by-link modal over Home.
    pub fn open_join(&mut self) {
        self.screen = Screen::Home;
        self.palette_open = false;
        self.create_open = false;
        self.join_error = None;
        self.join_open = true;
    }

    /// Drives the state to a named scene so a section can be rendered directly,
    /// without clicking through the UI. The spec is a set of dot/comma/space
    /// separated tokens, applied in order; unknown tokens are ignored so a typo
    /// degrades to "close enough" rather than a crash.
    ///
    /// Screens: `home` `room` `friends` `settings` `onboarding`.
    /// Overlays and variants: `palette` (command palette), `theatre` (room,
    /// sidebar hidden), `motion` (new-room Motion profile).
    pub fn apply_scene(&mut self, spec: &str) {
        for token in spec.split(['.', ',', ' ', ':']).filter(|t| !t.is_empty()) {
            match token.to_ascii_lowercase().as_str() {
                "home" => self.screen = Screen::Home,
                "room" => self.screen = Screen::Room,
                "friends" => self.screen = Screen::Friends,
                "settings" => self.screen = Screen::Settings,
                "onboarding" => self.screen = Screen::Onboarding,
                "palette" => self.palette_open = true,
                "create" => self.create_open = true,
                "join" => self.join_open = true,
                "chat" => self.room_tab = RoomTab::Chat,
                "diagnostics" => self.room_tab = RoomTab::Diagnostics,
                "theatre" | "theater" => self.theatre = true,
                "motion" => self.new_room_motion = true,
                "text" => self.new_room_motion = false,
                other => eprintln!("clarity: unknown scene token `{other}` ignored"),
            }
        }
    }
}
