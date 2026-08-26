//! Clarity desktop — a native egui client shell for the Clarity Share design.
//!
//! This renders the design's window chrome, sidebar, and screens. Screen
//! content is presentational for now; the Room view is the seam where the real
//! `clarity-client` sessions and live video will attach.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod chrome;
mod presence;
mod presenter;
mod room;
mod screens;
mod state;
mod theme;
mod ui;
mod viewer;

use std::sync::Arc;

use clarity_client::presenter::PresenterCommand as PresenterSessionCommand;
use clarity_client::signaling::SessionIdentity;
use clarity_client::viewer::ViewerCommand as ViewerSessionCommand;
use clarity_protocol::RoomAccessPolicy;
use eframe::egui;
use state::{AppState, RoomAccess, Screen};
use theme::Palette;

fn main() -> eframe::Result<()> {
    let mut store = match clarity_identity::Store::open() {
        Ok(store) => store,
        Err(error) => {
            eprintln!("clarity: {error}");
            std::process::exit(1);
        }
    };
    if std::env::var_os("CLARITY_SEED_DEMO").is_some() {
        seed_demo(&mut store);
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1360.0, 872.0])
            .with_min_inner_size([1040.0, 680.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_title("Clarity"),
        ..Default::default()
    };
    eframe::run_native(
        "Clarity",
        options,
        Box::new(move |cc| {
            theme::install_fonts(&cc.egui_ctx);
            configure_style(&cc.egui_ctx, &Palette::clarity());
            let mut state = AppState::new(store);
            if let Some(scene) = std::env::var_os("CLARITY_SCENE") {
                state.apply_scene(&scene.to_string_lossy());
            }
            if std::env::var_os("CLARITY_SEED_DEMO").is_some() {
                seed_demo_presence(&mut state);
            }
            if std::env::var_os("CLARITY_SEED_PRESENT").is_some() {
                seed_demo_presenter(&mut state);
            }
            // Dev/screenshot aid: auto-join a room by URL on launch.
            if let Some(url) = std::env::var_os("CLARITY_VIEW_URL") {
                state.join_room(url.to_string_lossy().into_owned());
            }
            // Dev/screenshot aid: open a room sharing a synthetic source on
            // launch (the link reads the same env to pick the source).
            if std::env::var_os("CLARITY_PRESENT_SYNTHETIC").is_some()
                && state.store.identity.is_some()
            {
                state.open_room();
            }
            Ok(Box::new(ClarityApp::new(state)))
        }),
    )
}

struct ClarityApp {
    pal: Palette,
    state: AppState,
    capture: capture::Capture,
    /// One runtime shared by the presence and presenter connections, created on
    /// first async need.
    runtime: Option<tokio::runtime::Runtime>,
    presence: Option<presence::PresenceLink>,
    presenter: Option<presenter::PresenterLink>,
    viewer: Option<viewer::ViewerLink>,
    /// The room id last announced to presence. The server keeps the announced
    /// room's viewer count and sharing state current on its own, so one
    /// announcement per room is enough.
    announced_room: Option<String>,
}

impl ClarityApp {
    fn new(state: AppState) -> Self {
        Self {
            pal: Palette::clarity(),
            state,
            capture: capture::Capture::from_env(),
            runtime: None,
            presence: None,
            presenter: None,
            viewer: None,
            announced_room: None,
        }
    }

    /// A handle to the shared runtime, created on first use. `None` only if the
    /// runtime could not be built.
    fn runtime(&mut self) -> Option<tokio::runtime::Handle> {
        if self.runtime.is_none() {
            self.runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .ok();
        }
        self.runtime.as_ref().map(|rt| rt.handle().clone())
    }

    /// Screenshot mode renders a static frame, so it must not open network
    /// connections.
    fn offline_mode(&self) -> bool {
        std::env::var_os("CLARITY_SHOT").is_some()
    }

    /// Starts the presence connection once an identity exists, and keeps its
    /// subscription and the presence view current.
    fn drive_presence(&mut self, ctx: &egui::Context) {
        if self.offline_mode() {
            return;
        }
        if self.presence.is_none()
            && self.state.store.identity.is_some()
            && let Some(handle) = self.runtime()
        {
            self.presence = presence::PresenceLink::start(&handle, ctx, &self.state.store);
        }
        if let Some(link) = &mut self.presence {
            link.pump(&mut self.state.presence_view);
            link.sync(presence::contact_codes(&self.state.store));
        }
        self.confirm_seen_contacts();
        self.expire_invites();
    }

    /// Marks pending contacts confirmed once presence reveals them. The server
    /// only reveals a friend when the pair is mutual, so a sighting is the
    /// confirmation the contact file is waiting for (the web client does the
    /// same in `confirmSeenContacts`).
    fn confirm_seen_contacts(&mut self) {
        let mut confirmed_any = false;
        for code in self.state.presence_view.friends.keys() {
            let pending = self
                .state
                .store
                .contacts
                .iter()
                .any(|contact| contact.code == *code && contact.pending);
            if pending {
                self.state.store.contacts.confirm(code);
                confirmed_any = true;
            }
        }
        if confirmed_any {
            let _ = self.state.store.persist_contacts();
        }
    }

    /// Drops pending invites that have aged out. The next frame's
    /// [`presence::PresenceLink::sync`] sees the smaller contact set and
    /// resubscribes, which withdraws the request server-side; the web client
    /// sweeps on the same TTL.
    fn expire_invites(&mut self) {
        if self.state.store.contacts.expire_invites() {
            let _ = self.state.store.persist_contacts();
        }
    }

    /// Handles room and sharing requests, forwards chat, and pumps the
    /// presenter session, announcing the hosted room to friends through
    /// presence.
    fn drive_presenter(&mut self, ctx: &egui::Context) {
        if self.offline_mode() {
            return;
        }
        for command in std::mem::take(&mut self.state.presenter_commands) {
            match command {
                state::PresenterCommand::OpenRoom => self.open_room(ctx),
                state::PresenterCommand::StartShare | state::PresenterCommand::ChangeSource => {
                    if let Some(link) = &self.presenter {
                        link.share(share_source());
                    }
                }
                state::PresenterCommand::PauseShare => {
                    self.presenter_command(PresenterSessionCommand::PauseShare);
                }
                state::PresenterCommand::ResumeShare => {
                    self.presenter_command(PresenterSessionCommand::ResumeShare);
                }
                state::PresenterCommand::StopShare => {
                    self.presenter_command(PresenterSessionCommand::StopShare);
                }
                state::PresenterCommand::CloseRoom => {
                    self.state.presenter_view.status = "Closing the room…".to_owned();
                    self.presenter_command(PresenterSessionCommand::CloseRoom);
                }
                state::PresenterCommand::Leave => self.detach_presenter(),
                state::PresenterCommand::Approve(peer_id) => {
                    self.presenter_command(PresenterSessionCommand::ApproveViewer(peer_id));
                }
                state::PresenterCommand::Deny(peer_id) => {
                    self.presenter_command(PresenterSessionCommand::RejectViewer(peer_id));
                }
            }
        }
        if let Some(link) = &self.presenter {
            for text in self.state.chat_out.drain(..) {
                link.chat(text);
            }
        }
        if let Some(link) = &mut self.presenter {
            link.pump(ctx, &mut self.state.presenter_view);
        }
        self.state.presenter_view.active = self.presenter.is_some();
        self.announce_hosting();
    }

    /// Creates the room named by the create modal's choices and hosts it,
    /// starting idle (unless a dev aid asks for an immediate synthetic share).
    fn open_room(&mut self, ctx: &egui::Context) {
        let Some(handle) = self.runtime() else {
            return;
        };
        let mut config = presenter::PresenterConfig::from_settings(&self.state.store.settings);
        config.expires_in_seconds = self.state.new_room_expiry.seconds();
        config.display_name = self
            .state
            .store
            .identity
            .as_ref()
            .map(|identity| identity.display_name().to_owned());
        match self.state.new_room_access {
            RoomAccess::AnyoneWithLink => {
                config.access_policy = RoomAccessPolicy::Public;
                config.auto_approve = true;
            }
            RoomAccess::AskFirst => {
                config.access_policy = RoomAccessPolicy::ApprovalRequired;
                config.auto_approve = false;
            }
            RoomAccess::FriendsOnly => {
                config.access_policy = RoomAccessPolicy::FriendsOnly;
                config.auto_approve = true;
                config.allowed_friend_codes = self
                    .state
                    .store
                    .contacts
                    .active()
                    .map(|contact| contact.code.clone())
                    .collect();
            }
        }
        if std::env::var_os("CLARITY_PRESENT_SYNTHETIC").is_some() {
            config.initial_source = Some(presenter::Source::Synthetic);
        }
        self.state.presenter_view = state::PresenterView {
            active: true,
            status: "Creating room…".to_owned(),
            target_ceiling_kbps: presenter::bitrate_for(config.profile),
            ..Default::default()
        };
        self.announced_room = None;
        self.presenter = Some(presenter::PresenterLink::start(&handle, ctx, config));
    }

    fn presenter_command(&self, command: PresenterSessionCommand) {
        if let Some(link) = &self.presenter {
            link.command(command);
        }
    }

    /// Detaches from the hosted room. Dropping the link drops the session's
    /// command channel, which the session treats as Leave: the room stays open
    /// and resumable on the server.
    fn detach_presenter(&mut self) {
        self.presenter = None;
        self.state.presenter_view = state::PresenterView::default();
        if self.announced_room.take().is_some()
            && let Some(presence) = &self.presence
        {
            presence.announce(None);
        }
        self.state.go(state::Screen::Home);
    }

    /// Handles join/leave requests and pumps the viewing session, uploading the
    /// latest decoded frame to a texture (or, behind `CLARITY_NATIVE_VIDEO=1`,
    /// rendering on a native subsurface below the window).
    fn drive_viewer(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        // The viewer runs during a screenshot only when explicitly asked to join
        // a URL, so a live capture can show real video.
        if self.offline_mode() && std::env::var_os("CLARITY_VIEW_URL").is_none() {
            return;
        }
        for command in std::mem::take(&mut self.state.viewer_commands) {
            match command {
                state::ViewerCommand::Join(url) => self.join_room(ctx, frame, &url),
                state::ViewerCommand::Leave => {
                    // Dropping the link drops the command channel; the session
                    // announces the leave and only this viewer is removed.
                    self.viewer = None;
                    self.state.viewer_view = state::ViewerView::default();
                    self.state.go(state::Screen::Home);
                }
                state::ViewerCommand::SetVolume(level) => {
                    if let Some(link) = &self.viewer {
                        link.command(ViewerSessionCommand::SetVolume(level));
                    }
                }
                state::ViewerCommand::SetName(name) => {
                    if let Some(link) = &self.viewer {
                        link.command(ViewerSessionCommand::SetDisplayName(name));
                    }
                }
                state::ViewerCommand::RestartIce => {
                    if let Some(link) = &self.viewer {
                        link.command(ViewerSessionCommand::RestartIce);
                    }
                }
            }
        }
        if self.presenter.is_none() {
            if let Some(link) = &self.viewer {
                for text in self.state.chat_out.drain(..) {
                    link.chat(text);
                }
            } else {
                self.state.chat_out.clear();
            }
        }
        if let Some(link) = &mut self.viewer {
            link.pump(ctx, &mut self.state.viewer_view);
        }
        self.state.viewer_view.active = self.viewer.is_some();
    }

    fn join_room(&mut self, ctx: &egui::Context, frame: &eframe::Frame, url: &str) {
        let Some(handle) = self.runtime() else {
            return;
        };
        self.state.viewer_view = state::ViewerView {
            active: true,
            status: "Connecting…".to_owned(),
            presenter_connected: true,
            room_code: clarity_client::invite::parse_invitation(url)
                .ok()
                .map(|invitation| invitation.room_id),
            ..Default::default()
        };
        let config = viewer::ViewerConfig {
            display_name: self
                .state
                .store
                .identity
                .as_ref()
                .map(|identity| identity.display_name().to_owned()),
            identity: session_identity(&self.state.store),
            force_relay: self.state.store.settings.always_relay,
        };
        let link = viewer::ViewerLink::start(&handle, ctx, url, config, native_handle(frame));
        self.viewer = Some(link);
    }

    /// Announces the hosted room to presence once it opens (and withdraws it
    /// when it ends), so mutually-added friends see it live. The server tracks
    /// the announced room's viewer count and sharing state itself afterwards.
    fn announce_hosting(&mut self) {
        let view = &self.state.presenter_view;
        let Some(presence) = &self.presence else {
            return;
        };
        let hosting = (view.active && view.open && !view.ended)
            .then(|| {
                view.room_id
                    .clone()
                    .zip(view.viewer_url.clone())
                    .zip(view.presenter_secret.clone())
            })
            .flatten();
        match hosting {
            Some(((room_id, viewer_url), presenter_secret)) => {
                if self.announced_room.as_ref() != Some(&room_id) {
                    presence.announce(Some(clarity_client::presence::HostingAnnouncement {
                        room: clarity_protocol::HostedRoom {
                            room_id: room_id.clone(),
                            viewer_url,
                            viewer_count: view.connected_viewers() as u32,
                            sharing_state: view.sharing,
                        },
                        presenter_secret,
                    }));
                    self.announced_room = Some(room_id);
                }
            }
            None => {
                if self.announced_room.take().is_some() {
                    presence.announce(None);
                }
            }
        }
    }
}

impl eframe::App for ClarityApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Transparent so the rounded window corners read against the desktop.
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drive_presence(&ctx);
        self.drive_presenter(&ctx);
        self.drive_viewer(&ctx, frame);
        handle_shortcuts(&ctx, &mut self.state);

        // While native video plays, the viewer stage is a transparent hole the
        // subsurface below shows through; everything opaque must paint around
        // it. The rect is last frame's layout, which only lags during a resize.
        let hole = (self.state.screen == Screen::Room && self.state.viewer_view.native_video)
            .then_some(self.state.viewer_view.stage_rect)
            .flatten();

        // Rounded window body painted behind the panels.
        let painter = ctx.layer_painter(egui::LayerId::background());
        match hole {
            Some(hole) => {
                // Four rects can't round corners, so the body loses its corner
                // rounding while a native room is on screen — acceptable.
                crate::ui::fill_around(&painter, ui.max_rect(), hole, self.pal.window);
                painter.rect_stroke(
                    ui.max_rect(),
                    egui::CornerRadius::same(12),
                    egui::Stroke::new(1.0_f32, self.pal.border),
                    egui::StrokeKind::Inside,
                );
            }
            None => {
                painter.rect(
                    ui.max_rect(),
                    egui::CornerRadius::same(12),
                    self.pal.window,
                    egui::Stroke::new(1.0_f32, self.pal.border),
                    egui::StrokeKind::Inside,
                );
            }
        }

        chrome::title_bar(ui, &self.pal, &mut self.state);

        if self.state.shows_sidebar() {
            chrome::sidebar(ui, &self.pal, &mut self.state);
        }

        // With a hole, nothing opaque may cover the stage: the room's own
        // panels fill themselves, and `viewer_stage` covers the rest.
        let central_frame = if hole.is_some() {
            egui::Frame::NONE
        } else {
            egui::Frame::NONE.fill(self.pal.window)
        };
        egui::CentralPanel::default()
            .frame(central_frame)
            .show(ui, |ui| match self.state.screen {
                Screen::Home => screens::home(ui, &self.pal, &mut self.state),
                Screen::Room => room::view(ui, &self.pal, &mut self.state),
                Screen::Friends => screens::friends(ui, &self.pal, &mut self.state),
                Screen::Settings => screens::settings(ui, &self.pal, &mut self.state),
                Screen::Onboarding => screens::onboarding(ui, &self.pal, &mut self.state),
            });

        if let Some(link) = &self.viewer {
            link.sync_stage(hole);
        }

        if self.state.create_open {
            screens::create_room_modal(&ctx, &self.pal, &mut self.state);
        }
        if self.state.join_open {
            screens::join_room_modal(&ctx, &self.pal, &mut self.state);
        }
        if self.state.palette_open {
            screens::command_palette(&ctx, &self.pal, &mut self.state);
        }

        self.capture.tick(&ctx);
    }
}

/// The capture the presenter shares: the portal picker normally, a synthetic
/// pattern behind `CLARITY_PRESENT_SYNTHETIC` (dev/screenshot aid).
fn share_source() -> presenter::Source {
    if std::env::var_os("CLARITY_PRESENT_SYNTHETIC").is_some() {
        presenter::Source::Synthetic
    } else {
        presenter::Source::Screen
    }
}

/// Proof material for identity-checked (friends-only) rooms, from the local
/// identity. `None` before onboarding.
fn session_identity(store: &clarity_identity::Store) -> Option<SessionIdentity> {
    let identity = store.identity.as_ref()?;
    let signer = identity.clone();
    Some(SessionIdentity {
        public_key: identity.public_key().to_vec(),
        sign: Arc::new(move |message: &[u8]| signer.sign(message)),
    })
}

/// Populates an empty store with a demo identity and a couple of pending
/// contacts, for screenshots and manual testing (`CLARITY_SEED_DEMO=1`). A
/// no-op once an identity exists.
fn seed_demo(store: &mut clarity_identity::Store) {
    if store.identity.is_some() {
        return;
    }
    let Ok(identity) = clarity_identity::Identity::create("Jamie", "Studio Mac") else {
        return;
    };
    let own = identity.friend_code();
    for name in ["Mara Kovács", "Dan Reyes"] {
        if let Ok(friend) = clarity_identity::Identity::create(name, "device") {
            let _ = store.contacts.add(&friend.friend_code(), name, &own);
        }
    }
    // Confirm the first contact so the demo shows an active friend alongside a
    // pending one.
    let first = store.contacts.iter().next().map(|c| c.code.clone());
    if let Some(first) = first {
        store.contacts.confirm(&first);
    }
    store.identity = Some(identity);
    let _ = store.persist_identity();
    let _ = store.persist_contacts();
}

/// Injects a live presenter session for demo screenshots of the Room while
/// sharing (`CLARITY_SEED_PRESENT=1`), without a running server.
fn seed_demo_presenter(state: &mut AppState) {
    let mut viewers = std::collections::HashMap::new();
    viewers.insert(
        "v1".to_owned(),
        state::ViewerCard {
            name: "June Tan".to_owned(),
            connected: true,
            bitrate_kbps: Some(2_100),
            round_trip_ms: Some(12.0),
            packets_lost: Some(0),
            packets_sent: Some(184_000),
            target_kbps: 6_000,
            codec: Some("AV1".to_owned()),
        },
    );
    viewers.insert(
        "v2".to_owned(),
        state::ViewerCard {
            name: "Ade Okafor".to_owned(),
            connected: true,
            bitrate_kbps: Some(1_450),
            round_trip_ms: Some(96.0),
            packets_lost: Some(1_400),
            packets_sent: Some(100_000),
            target_kbps: 2_500,
            codec: Some("VP8".to_owned()),
        },
    );
    // A minute of samples so the diagnostics sparkline has a shape.
    let mut bitrate_history = state::BitrateHistory::default();
    let now = std::time::Instant::now();
    for i in 0..60u32 {
        let age = std::time::Duration::from_secs((60 - i).into());
        if let Some(at) = now.checked_sub(age) {
            let wave = ((i as f32 / 6.0).sin() * 800.0) as i32;
            bitrate_history.record_at(at, (3_400 + wave).max(0) as u32);
        }
    }
    let messages = vec![
        state::ChatMessage {
            from: "June Tan".to_owned(),
            text: "the left column is dropping frames on scroll".to_owned(),
            own: false,
        },
        state::ChatMessage {
            from: "You".to_owned(),
            text: "switching to text mode — better?".to_owned(),
            own: true,
        },
        state::ChatMessage {
            from: "Ade Okafor".to_owned(),
            text: "much clearer now, keep it there".to_owned(),
            own: false,
        },
    ];
    state.presenter_view = state::PresenterView {
        active: true,
        open: true,
        sharing: clarity_protocol::SharingState::Live,
        status: "Sharing your screen".to_owned(),
        room_id: Some("YCIUG6X8".to_owned()),
        viewer_url: Some("https://clarity.example/r/YCIUG6X8#secret".to_owned()),
        countdown: Some(state::RoomCountdown {
            seconds: 3 * 60 * 60 + 12 * 60,
            at: std::time::Instant::now(),
        }),
        viewers,
        requests: vec![state::JoinRequest {
            peer_id: "p1".to_owned(),
            name: Some("Priya".to_owned()),
            friend_code: Some("clr-9QF2-X1LM".to_owned()),
        }],
        messages,
        bitrate_history,
        target_ceiling_kbps: 6_000,
        ..Default::default()
    };
    state.go(state::Screen::Room);
}

/// Injects a live friend into the presence view for demo screenshots, so the
/// populated "Live now"/room UI is exercised without a running server.
fn seed_demo_presence(state: &mut AppState) {
    state.presence_view.connected = true;
    let Some(code) = state
        .store
        .contacts
        .active()
        .next()
        .map(|contact| contact.code.clone())
    else {
        return;
    };
    state.presence_view.friends.insert(
        code.clone(),
        clarity_protocol::FriendPresence {
            code,
            online: true,
            hosting: Some(clarity_protocol::HostedRoom {
                room_id: "demo-room".to_owned(),
                viewer_url: "https://clarity.example/r/demo-room#secret".to_owned(),
                viewer_count: 4,
                sharing_state: clarity_protocol::SharingState::Live,
            }),
            last_seen_seconds_ago: None,
        },
    );
}

/// Window handles for the native video path, behind `CLARITY_NATIVE_VIDEO=1`.
/// `None` — the default, and any non-Wayland session — keeps the viewer on
/// the frame-sink → egui-texture path.
fn native_handle(frame: &eframe::Frame) -> Option<clarity_client::NativeHandle> {
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    if std::env::var("CLARITY_NATIVE_VIDEO").as_deref() != Ok("1") {
        return None;
    }
    let display = frame.display_handle().ok()?.as_raw();
    let window = frame.window_handle().ok()?.as_raw();
    match (display, window) {
        (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(window)) => {
            Some(clarity_client::NativeHandle::Wayland {
                display: display.display.as_ptr(),
                surface: window.surface.as_ptr(),
            })
        }
        _ => None,
    }
}

fn handle_shortcuts(ctx: &egui::Context, state: &mut AppState) {
    // Read focus first: `memory()` takes the same context lock that `input_mut()`
    // holds, so reading it inside the closure would deadlock.
    let typing = ctx.memory(|m| m.focused().is_some());
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::COMMAND, egui::Key::K) {
            state.open_palette();
        }
        // ⌘N opens the create-room modal, unless typing in a field.
        if !typing && i.consume_key(egui::Modifiers::COMMAND, egui::Key::N) {
            state.open_create();
        }
        // The palette's advertised shortcuts: ⌘⇧A → Friends, ⌘, → Settings.
        if i.consume_key(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::A,
        ) {
            state.go(Screen::Friends);
        }
        if i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma) {
            state.go(Screen::Settings);
        }
        if i.key_pressed(egui::Key::Escape) {
            state.palette_open = false;
            state.create_open = false;
            state.join_open = false;
            state.theatre = false;
            state.name_edit_open = false;
        }
        // `T` toggles theatre in the room, when not typing in a field.
        if state.screen == Screen::Room
            && !typing
            && i.consume_key(egui::Modifiers::NONE, egui::Key::T)
        {
            state.theatre = !state.theatre;
        }
    });
}

/// Baseline egui styling: dark, tight spacing, the design's text sizes. Most
/// surfaces are drawn explicitly, so this only sets shared defaults.
fn configure_style(ctx: &egui::Context, pal: &Palette) {
    use egui::{FontFamily, FontId, TextStyle};
    ctx.all_styles_mut(|style| {
        style.visuals.dark_mode = true;
        style.visuals.override_text_color = Some(pal.text);
        style.visuals.window_fill = pal.window;
        style.visuals.panel_fill = pal.window;
        style.visuals.extreme_bg_color = pal.input;
        style.visuals.selection.bg_fill = pal.accent_dim;
        style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, pal.accent);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        style.text_styles = [
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(12.5, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(11.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(11.0, FontFamily::Monospace),
            ),
            (
                TextStyle::Heading,
                FontId::new(28.0, FontFamily::Proportional),
            ),
        ]
        .into();
    });
}
