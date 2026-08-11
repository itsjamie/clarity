//! Screen bodies rendered into the central area: Home, Friends, Settings, and
//! Onboarding, plus the command palette and the create-room modal overlays.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, Pos2, Rect, Sense, Stroke, vec2,
};

use crate::state::{AppState, Screen};
use crate::theme::Palette;
use crate::ui::{
    accent_button, avatar, card, checkbox_row, chevron_down, field_label, heading, mono_text,
    paint_hatch, section_label, strong, text,
};

pub fn home(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::NONE
                .inner_margin(Margin {
                    left: 40,
                    right: 40,
                    top: 34,
                    bottom: 44,
                })
                .show(ui, |ui| home_body(ui, pal, state));
        });
}

/// The Home body: a single column of live-room cards, or an empty state. There
/// is no inline create form; a room is created from the modal (`open_create`),
/// launched from the sidebar, ⌘N, or the palette.
fn home_body(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    let rooms: Vec<(String, String, u32, clarity_protocol::SharingState)> = state
        .presence_view
        .live()
        .map(|(code, room)| {
            let name = state
                .store
                .contacts
                .name_of(code)
                .unwrap_or(code)
                .to_owned();
            (
                name,
                room.viewer_url.clone(),
                room.viewer_count,
                room.sharing_state,
            )
        })
        .collect();

    let w = ui.available_width().min(940.0);
    ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::top_down(Align::Min), |ui| {
        if rooms.is_empty() {
            no_rooms_card(ui, pal, state);
        } else {
            for (index, (name, url, here, sharing)) in rooms.iter().enumerate() {
                if index > 0 {
                    ui.add_space(14.0);
                }
                if *sharing == clarity_protocol::SharingState::Idle {
                    idle_room_card(ui, pal, state, name, url, *here);
                } else {
                    live_room_card(ui, pal, state, name, url, *here, *sharing);
                }
            }
        }
    });
}

fn no_rooms_card(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    let connected = state.presence_view.connected;
    let mut create = false;
    card(ui, pal.card_alt, pal.border, Margin::symmetric(20, 36), |ui| {
        ui.vertical_centered(|ui| {
            ui.label(strong("Nothing live yet", 16.0, pal.text));
            ui.add_space(8.0);
            let sub = if connected {
                "Create a room to share your screen, or wait for a friend to share — theirs appears here."
            } else {
                "Connecting to your server…"
            };
            ui.label(text(sub, 12.5, pal.text_dim));
            ui.add_space(18.0);
            create = accent_button(ui, pal, "Create a room", 38.0)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked();
        });
    });
    if create {
        state.open_create();
    }
}

fn live_room_card(
    ui: &mut egui::Ui,
    pal: &Palette,
    state: &mut AppState,
    name: &str,
    url: &str,
    here: u32,
    sharing: clarity_protocol::SharingState,
) {
    let live = sharing == clarity_protocol::SharingState::Live;
    Frame::NONE
        .fill(pal.card)
        .stroke(Stroke::new(1.0_f32, pal.accent_dim.gamma_multiply(2.2)))
        .corner_radius(CornerRadius::same(12))
        .show(ui, |ui| {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 212.0), Sense::hover());
            paint_hatch(ui.painter(), rect, pal.raised, pal.card, 6.0);
            if live {
                live_badge(ui, rect.left_top() + vec2(14.0, 12.0), pal);
            }
            ui.painter().text(
                rect.left_bottom() + vec2(14.0, -12.0),
                egui::Align2::LEFT_BOTTOM,
                "live preview",
                egui::FontId::new(10.0, crate::theme::mono()),
                pal.text_dim,
            );

            Frame::NONE
                .inner_margin(Margin {
                    left: 18,
                    right: 18,
                    top: 16,
                    bottom: 18,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing = vec2(0.0, 5.0);
                            let headline = match sharing {
                                clarity_protocol::SharingState::Live => {
                                    format!("{name} is sharing")
                                }
                                clarity_protocol::SharingState::Paused => {
                                    format!("{name} paused their share")
                                }
                                clarity_protocol::SharingState::Idle => {
                                    format!("{name} has a room open")
                                }
                            };
                            let mut job = egui::text::LayoutJob::default();
                            job.append(
                                &headline,
                                0.0,
                                egui::TextFormat::simple(
                                    egui::FontId::new(16.0, crate::theme::medium()),
                                    pal.text_bright,
                                ),
                            );
                            ui.label(job);
                            let watchers = match here {
                                0 => "no one watching yet · peer to peer".to_owned(),
                                1 => "1 watching · peer to peer".to_owned(),
                                n => format!("{n} watching · peer to peer"),
                            };
                            ui.label(text(&watchers, 12.5, pal.text_muted));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if accent_button(ui, pal, "Join", 36.0).clicked() {
                                state.join_room(url.to_owned());
                            }
                        });
                    });
                });
        });
}

/// The design's compact row for a friend's open-but-idle room: no preview,
/// a plain "Open, nobody sharing yet" line, and still joinable (the room —
/// and its chat — outlives sharing).
fn idle_room_card(
    ui: &mut egui::Ui,
    pal: &Palette,
    state: &mut AppState,
    name: &str,
    url: &str,
    here: u32,
) {
    card(ui, pal.card_alt, pal.border, Margin::symmetric(18, 16), |ui| {
        ui.horizontal(|ui| {
            avatar(ui, 34.0, &crate::ui::initials(name), pal.raised, pal.text);
            ui.add_space(14.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 4.0);
                ui.label(strong(format!("{name}'s room"), 14.0, pal.text));
                let sub = match here {
                    0 => "Open, nobody sharing yet".to_owned(),
                    1 => "Open, nobody sharing yet · 1 here".to_owned(),
                    n => format!("Open, nobody sharing yet · {n} here"),
                };
                ui.label(text(sub, 12.0, pal.text_dim));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if crate::ui::neutral_button(ui, pal, "Join", 34.0)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    state.join_room(url.to_owned());
                }
            });
        });
    });
}

/// The create-room modal — the design's create flow, floating over Home. Opened
/// from the sidebar, ⌘N, or the palette; picks the capture profile and opens a
/// room. Dismissed by the scrim, the close button, or Escape.
pub fn create_room_modal(ctx: &egui::Context, pal: &Palette, state: &mut AppState) {
    // The full window (viewport minus any notches), so the scrim dims
    // everything behind the modal.
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new("create-scrim"))
        .fixed_pos(screen.left_top())
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            let scrim = ui.allocate_rect(screen, Sense::click());
            ui.painter().rect_filled(
                screen,
                CornerRadius::same(12),
                Color32::from_black_alpha(150),
            );
            if scrim.clicked() {
                state.create_open = false;
            }
        });

    egui::Window::new("create-room")
        .title_bar(false)
        .order(egui::Order::Foreground)
        .resizable(false)
        .fixed_size(vec2(460.0, 0.0))
        .anchor(egui::Align2::CENTER_TOP, vec2(0.0, 96.0))
        .frame(
            Frame::NONE
                .fill(pal.card)
                .stroke(Stroke::new(1.0_f32, pal.border_strong))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(22)),
        )
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 14.0);
            ui.horizontal(|ui| {
                ui.label(mono_text("CREATE A ROOM", 10.0, pal.text_dim));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if modal_close(ui, pal).clicked() {
                        state.create_open = false;
                    }
                });
            });

            field_label(ui, pal, "Who can join");
            let access_labels: Vec<&str> =
                crate::state::RoomAccess::ALL.iter().map(|a| a.label()).collect();
            let access_idx = crate::state::RoomAccess::ALL
                .iter()
                .position(|a| *a == state.new_room_access)
                .unwrap_or(0);
            if let Some(i) = dropdown(ui, pal, "create-access", &access_labels, access_idx) {
                state.new_room_access = crate::state::RoomAccess::ALL[i];
            }

            field_label(ui, pal, "Capture profile");
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 8.0) / 2.0;
                profile_button(ui, pal, w, "Text", "Sharp at 30 fps", !state.new_room_motion, || {
                    state.new_room_motion = false;
                });
                ui.add_space(8.0);
                profile_button(ui, pal, w, "Motion", "Smooth at 60 fps", state.new_room_motion, || {
                    state.new_room_motion = true;
                });
            });

            field_label(ui, pal, "Room expires in");
            let expiry_labels: Vec<&str> =
                crate::state::RoomExpiry::ALL.iter().map(|e| e.label()).collect();
            let expiry_idx = crate::state::RoomExpiry::ALL
                .iter()
                .position(|e| *e == state.new_room_expiry)
                .unwrap_or(0);
            if let Some(i) = dropdown(ui, pal, "create-expiry", &expiry_labels, expiry_idx) {
                state.new_room_expiry = crate::state::RoomExpiry::ALL[i];
            }

            if let Some(error) = &state.create_error {
                ui.label(text(error, 11.5, pal.red));
            }

            ui.add_space(2.0);
            if accent_button(ui, pal, "Open room", 44.0)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                try_open_room(state);
            }
        });
}

/// Validates the create form and opens an idle room. Sharing starts later,
/// from inside the room. A friends-only room needs at least one confirmed
/// contact to build the allowlist from.
fn try_open_room(state: &mut AppState) {
    if state.new_room_access == crate::state::RoomAccess::FriendsOnly
        && state.store.contacts.active().next().is_none()
    {
        state.create_error = Some(
            "No confirmed friends to allow yet. Add a friend first, or pick another access rule."
                .to_owned(),
        );
        return;
    }
    // Apply the chosen profile so the capture matches the modal.
    state.store.settings.capture_profile = if state.new_room_motion {
        clarity_identity::CaptureProfile::Motion
    } else {
        clarity_identity::CaptureProfile::Text
    };
    let _ = state.store.persist_settings();
    state.create_error = None;
    state.create_open = false;
    state.open_room();
}

/// The join-by-link modal: paste a room link and join it. Validates the link
/// before navigating, so a bad paste shows an inline error instead of dropping
/// into a failing session. Opened from the sidebar; dismissed by the scrim, the
/// close button, or Escape.
pub fn join_room_modal(ctx: &egui::Context, pal: &Palette, state: &mut AppState) {
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new("join-scrim"))
        .fixed_pos(screen.left_top())
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            let scrim = ui.allocate_rect(screen, Sense::click());
            ui.painter().rect_filled(
                screen,
                CornerRadius::same(12),
                Color32::from_black_alpha(150),
            );
            if scrim.clicked() {
                state.join_open = false;
            }
        });

    egui::Window::new("join-room")
        .title_bar(false)
        .order(egui::Order::Foreground)
        .resizable(false)
        .fixed_size(vec2(460.0, 0.0))
        .anchor(egui::Align2::CENTER_TOP, vec2(0.0, 96.0))
        .frame(
            Frame::NONE
                .fill(pal.card)
                .stroke(Stroke::new(1.0_f32, pal.border_strong))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(22)),
        )
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 14.0);
            ui.horizontal(|ui| {
                ui.label(mono_text("JOIN A ROOM", 10.0, pal.text_dim));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if modal_close(ui, pal).clicked() {
                        state.join_open = false;
                    }
                });
            });

            field_label(ui, pal, "Room link");
            let entered = crate::ui::text_field(
                ui,
                pal,
                &mut state.join_draft,
                "https://clarity.example/r/…#key",
                44.0,
                13.0,
                true,
            )
            .lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter));

            if let Some(error) = &state.join_error {
                ui.label(text(error, 11.5, pal.red));
            } else {
                ui.label(text(
                    "Paste the link a friend shared. It opens their live room.",
                    11.5,
                    pal.text_dim,
                ));
            }

            let clicked = accent_button(ui, pal, "Join room", 44.0)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked();
            if clicked || entered {
                try_join(state);
            }
        });
}

/// Validates the join draft and either joins the room or records an error.
fn try_join(state: &mut AppState) {
    let link = state.join_draft.trim().to_owned();
    if link.is_empty() {
        state.join_error = Some("Paste a room link to join.".to_owned());
        return;
    }
    match clarity_client::invite::parse_invitation(&link) {
        Ok(_) => {
            state.join_open = false;
            state.join_error = None;
            state.join_draft.clear();
            state.join_room(link);
        }
        Err(_) => {
            state.join_error =
                Some("That doesn't look like a Clarity room link.".to_owned());
        }
    }
}

/// A small "×" close button for a modal's header.
fn modal_close(ui: &mut egui::Ui, pal: &Palette) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(24.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), Color32::from_white_alpha(14));
    }
    let c = rect.center();
    let color = if resp.hovered() { pal.text_bright } else { pal.text_dim };
    for d in [vec2(-4.0, -4.0), vec2(-4.0, 4.0)] {
        ui.painter()
            .line_segment([c + d, c - d], Stroke::new(1.4_f32, color));
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn faux_select(ui: &mut egui::Ui, pal: &Palette, value: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    let border = if resp.hovered() {
        pal.border_strong
    } else {
        pal.border
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        pal.window,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.left_center() + vec2(11.0, 0.0),
        egui::Align2::LEFT_CENTER,
        value,
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
        pal.text_bright,
    );
    chevron_down(ui.painter(), rect.right_center() - vec2(14.0, 0.0), pal.text_dim);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A real dropdown: the select field opens a popup list of `options` on click.
/// Returns the newly chosen index on the frame the selection changes. `id` must
/// be stable across frames so the open/closed state persists.
fn dropdown(
    ui: &mut egui::Ui,
    pal: &Palette,
    id: &str,
    options: &[&str],
    selected: usize,
) -> Option<usize> {
    let current = options.get(selected).copied().unwrap_or("");
    let field = faux_select(ui, pal, current);
    let mut chosen = None;
    egui::Popup::from_toggle_button_response(&field)
        .id(egui::Id::new(id))
        .width(field.rect.width())
        .gap(4.0)
        .frame(
            Frame::NONE
                .fill(pal.card)
                .stroke(Stroke::new(1.0_f32, pal.border_strong))
                .corner_radius(CornerRadius::same(9))
                .inner_margin(Margin::same(5)),
        )
        .show(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (i, option) in options.iter().enumerate() {
                if dropdown_item(ui, pal, option, i == selected).clicked() {
                    chosen = Some(i);
                }
            }
        });
    chosen
}

/// One row in a dropdown popup: hover highlight, accent fill when selected.
fn dropdown_item(ui: &mut egui::Ui, pal: &Palette, label: &str, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let bg = if selected {
        pal.accent_dim
    } else if resp.hovered() {
        Color32::from_white_alpha(12)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(7), bg);
    ui.painter().text(
        rect.left_center() + vec2(11.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
        if selected { pal.text_bright } else { pal.text },
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A field label above a dropdown; returns the newly chosen index on change.
fn labeled_dropdown(
    ui: &mut egui::Ui,
    pal: &Palette,
    label: &str,
    id: &str,
    options: &[&str],
    selected: usize,
) -> Option<usize> {
    field_label(ui, pal, label);
    ui.add_space(7.0);
    dropdown(ui, pal, id, options, selected)
}

fn profile_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    w: f32,
    title: &str,
    sub: &str,
    selected: bool,
    mut on_click: impl FnMut(),
) {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 58.0), Sense::click());
    let (fill, border) = if selected {
        (
            pal.accent_dim.gamma_multiply(0.75),
            pal.accent.gamma_multiply(0.55),
        )
    } else {
        (
            Color32::TRANSPARENT,
            if resp.hovered() {
                pal.border_strong
            } else {
                pal.border
            },
        )
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(9),
        fill,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    let title_c = if selected { pal.text_bright } else { pal.text };
    ui.painter().text(
        rect.left_top() + vec2(12.0, 11.0),
        egui::Align2::LEFT_TOP,
        title,
        egui::FontId::new(12.5, egui::FontFamily::Proportional),
        title_c,
    );
    ui.painter().text(
        rect.left_top() + vec2(12.0, 30.0),
        egui::Align2::LEFT_TOP,
        sub,
        egui::FontId::new(11.0, egui::FontFamily::Proportional),
        pal.text_muted,
    );
    if resp.clicked() {
        on_click();
    }
}

fn live_badge(ui: &mut egui::Ui, at: Pos2, pal: &Palette) {
    let text_str = "LIVE";
    let galley = ui.painter().layout_no_wrap(
        text_str.into(),
        egui::FontId::new(10.0, crate::theme::mono()),
        pal.text,
    );
    let w = galley.size().x + 26.0;
    let rect = Rect::from_min_size(at, vec2(w, 20.0));
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(255),
        Color32::from_black_alpha(200),
    );
    ui.painter()
        .circle_filled(rect.left_center() + vec2(11.0, 0.0), 2.5, pal.green);
    ui.painter().galley(
        rect.left_center() + vec2(18.0, -galley.size().y / 2.0),
        galley,
        pal.text,
    );
}

/// What clicking a palette row does.
#[derive(Clone)]
enum PaletteAction {
    Create,
    JoinByLink,
    Go(Screen),
    /// Join a room by its viewer URL.
    Join(String),
}

/// One palette row: its label (the filter target), a right-aligned mono hint,
/// and its action.
struct PaletteEntry {
    label: String,
    hint: String,
    action: PaletteAction,
}

/// True when `label` matches the palette query: case-insensitive substring, or
/// the query's characters appearing in order (a cheap fuzzy match).
fn palette_match(query: &str, label: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let label = label.to_lowercase();
    if label.contains(&query) {
        return true;
    }
    let mut chars = label.chars();
    'query: for wanted in query.chars().filter(|c| !c.is_whitespace()) {
        for candidate in chars.by_ref() {
            if candidate == wanted {
                continue 'query;
            }
        }
        return false;
    }
    true
}

/// The palette's candidate rows: the fixed actions, every room friends are
/// hosting right now, and the contact list. Owned strings so rendering can
/// mutate `state` afterwards.
fn palette_entries(state: &AppState) -> [(&'static str, Vec<PaletteEntry>); 3] {
    let modifier = crate::theme::mod_key();
    let actions = vec![
        PaletteEntry {
            label: "Create a room".to_owned(),
            hint: format!("{modifier} N"),
            action: PaletteAction::Create,
        },
        PaletteEntry {
            label: "Join a room by link".to_owned(),
            hint: String::new(),
            action: PaletteAction::JoinByLink,
        },
        PaletteEntry {
            label: "Add a friend".to_owned(),
            hint: format!("{modifier} Shift A"),
            action: PaletteAction::Go(Screen::Friends),
        },
        PaletteEntry {
            label: "Open settings".to_owned(),
            hint: format!("{modifier} ,"),
            action: PaletteAction::Go(Screen::Settings),
        },
    ];
    let rooms: Vec<PaletteEntry> = state
        .presence_view
        .live()
        .map(|(code, room)| {
            let name = state.store.contacts.name_of(code).unwrap_or(code);
            PaletteEntry {
                label: format!("Join {name}'s room"),
                hint: match room.sharing_state {
                    clarity_protocol::SharingState::Live => "live",
                    clarity_protocol::SharingState::Paused => "paused",
                    clarity_protocol::SharingState::Idle => "idle",
                }
                .to_owned(),
                action: PaletteAction::Join(room.viewer_url.clone()),
            }
        })
        .collect();
    let friends: Vec<PaletteEntry> = state
        .store
        .contacts
        .iter()
        .map(|contact| {
            let presence = state.presence_view.friends.get(&contact.code);
            let hint = if contact.pending {
                "pending".to_owned()
            } else if presence.is_some_and(|friend| friend.online) {
                "online".to_owned()
            } else if let Some(seconds) = presence.and_then(|f| f.last_seen_seconds_ago) {
                clarity_core::ago_compact(seconds)
            } else {
                "friend".to_owned()
            };
            // A friend hosting a room jumps straight into it; anyone else
            // lands on the Friends screen.
            let action = presence
                .and_then(|friend| friend.hosting.as_ref())
                .map(|room| PaletteAction::Join(room.viewer_url.clone()))
                .unwrap_or(PaletteAction::Go(Screen::Friends));
            PaletteEntry {
                label: contact.name.clone(),
                hint,
                action,
            }
        })
        .collect();
    [("Actions", actions), ("Live now", rooms), ("Friends", friends)]
}

pub fn command_palette(ctx: &egui::Context, pal: &Palette, state: &mut AppState) {
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new("palette-scrim"))
        .fixed_pos(screen.left_top())
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            let scrim = ui.allocate_rect(screen, Sense::click());
            ui.painter().rect_filled(
                screen,
                CornerRadius::same(12),
                Color32::from_black_alpha(150),
            );
            if scrim.clicked() {
                state.palette_open = false;
            }
        });

    let groups = palette_entries(state);
    let mut chosen: Option<PaletteAction> = None;
    egui::Window::new("palette")
        .title_bar(false)
        .order(egui::Order::Foreground)
        .resizable(false)
        .fixed_size(vec2(520.0, 0.0))
        .anchor(egui::Align2::CENTER_TOP, vec2(0.0, 120.0))
        .frame(
            Frame::NONE
                .fill(pal.card)
                .stroke(Stroke::new(1.0_f32, pal.border_strong))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(10)),
        )
        .show(ctx, |ui| {
            let edit = egui::TextEdit::singleline(&mut state.palette_query)
                .hint_text("Jump to a friend, room, or setting…")
                .desired_width(f32::INFINITY)
                .margin(Margin::symmetric(12, 10))
                .font(egui::FontId::new(14.0, egui::FontFamily::Proportional));
            ui.add(edit).request_focus();
            let mut any = false;
            for (title, entries) in &groups {
                let visible: Vec<&PaletteEntry> = entries
                    .iter()
                    .filter(|entry| palette_match(&state.palette_query, &entry.label))
                    .collect();
                if visible.is_empty() {
                    continue;
                }
                any = true;
                ui.add_space(8.0);
                section_label(ui, pal, title, None);
                ui.add_space(4.0);
                for entry in visible {
                    if palette_row(ui, pal, &entry.label, &entry.hint).clicked() {
                        chosen = Some(entry.action.clone());
                    }
                }
            }
            if !any {
                ui.add_space(8.0);
                Frame::NONE
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.label(text("Nothing matches.", 12.5, pal.text_dim));
                    });
            }
        });
    if let Some(action) = chosen {
        match action {
            PaletteAction::Create => state.open_create(),
            PaletteAction::JoinByLink => state.open_join(),
            PaletteAction::Go(screen) => state.go(screen),
            PaletteAction::Join(url) => state.join_room(url),
        }
    }
}

fn palette_row(ui: &mut egui::Ui, pal: &Palette, label: &str, hint: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), Color32::from_white_alpha(12));
    }
    ui.painter().text(
        rect.left_center() + vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
        pal.text,
    );
    ui.painter().text(
        rect.right_center() - vec2(10.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        hint,
        egui::FontId::new(10.5, crate::theme::mono()),
        pal.text_dim,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// --- Friends ---

pub fn friends(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    scrolled_page(ui, pal, |ui| {
        ui.label(heading("Add a friend", 30.0, pal.text_bright));
        ui.add_space(14.0);
        constrained(ui, 620.0, |ui| {
            ui.label(text(
                "Clarity has no accounts. Your identity lives on this device as a key pair; \
                 a friend code is its public half. Trade codes once and you'll see each \
                 other's rooms from then on.",
                14.0,
                pal.text_muted,
            ));
        });

        ui.add_space(30.0);
        constrained(ui, 900.0, |ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            ui.columns(2, |cols| {
                your_code_card(&mut cols[0], pal, state);
                their_code_card(&mut cols[1], pal, state);
            });
        });

        ui.add_space(34.0);
        let pending: Vec<clarity_identity::Contact> =
            state.store.contacts.pending().cloned().collect();
        constrained(ui, 900.0, |ui| {
            section_label(ui, pal, "Waiting on them", None);
            ui.add_space(10.0);
            if pending.is_empty() {
                ui.label(text(
                    "No invites waiting. Enter a friend's code above to send one.",
                    12.5,
                    pal.text_dim,
                ));
            } else {
                ui.spacing_mut().item_spacing.y = 8.0;
                let mut cancel = None;
                for contact in &pending {
                    // "code · sent 2h ago", falling back to "pending" for
                    // contacts saved before the added-at field existed.
                    let meta = match contact.added_seconds_ago() {
                        Some(seconds) => match clarity_core::ago_compact(seconds).as_str() {
                            "now" => format!("{} · sent just now", contact.code),
                            ago => format!("{} · sent {ago} ago", contact.code),
                        },
                        None => format!("{} · pending", contact.code),
                    };
                    if pending_invite(ui, pal, &contact.name, &meta) {
                        cancel = Some(contact.code.clone());
                    }
                }
                if let Some(code) = cancel {
                    state.store.contacts.remove(&code);
                    let _ = state.store.persist_contacts();
                }
            }
        });
    });
}

fn your_code_card(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    let code = state
        .store
        .identity
        .as_ref()
        .map(|identity| identity.friend_code())
        .unwrap_or_else(|| "clr-????-????".to_owned());
    card(ui, pal.card, pal.border, Margin::same(22), |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        section_label(ui, pal, "Your code", None);
        ui.add_space(16.0);
        code_plate(ui, pal, &code);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 8.0) / 2.0;
            if neutral_button_wide(ui, pal, w, "Copy code").clicked() {
                ui.ctx().copy_text(code.clone());
            }
            ui.add_space(8.0);
            if outline_button_wide(ui, pal, w, "Rotate").clicked() {
                if let Some(identity) = state.store.identity.as_mut() {
                    let _ = identity.rotate();
                }
                let _ = state.store.persist_identity();
            }
        });
        ui.add_space(16.0);
        ui.label(text(
            "Rotating invalidates the old code. Friends you've already added stay.",
            12.0,
            pal.text_dim,
        ));
    });
}

fn their_code_card(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    card(ui, pal.card, pal.border, Margin::same(22), |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        section_label(ui, pal, "Their code", None);
        ui.add_space(16.0);
        crate::ui::text_field(
            ui,
            pal,
            &mut state.friend_code_draft,
            "clr-XXXX-XXXX",
            52.0,
            17.0,
            true,
        );
        ui.add_space(12.0);
        field_label(ui, pal, "Name them (only you see this)");
        ui.add_space(7.0);
        crate::ui::text_field(ui, pal, &mut state.friend_name_draft, "e.g. Mara", 40.0, 13.0, false);
        if let Some(error) = &state.friend_error {
            ui.add_space(8.0);
            ui.label(text(error, 11.5, pal.red));
        }
        ui.add_space(16.0);
        if accent_button_wide(ui, pal, ui.available_width(), "Add friend", 42.0).clicked() {
            add_friend(state);
        }
    });
}

/// Adds the drafted friend code, clearing the inputs on success or surfacing the
/// reason on failure.
fn add_friend(state: &mut AppState) {
    let own = state
        .store
        .identity
        .as_ref()
        .map(|identity| identity.friend_code())
        .unwrap_or_default();
    let code = state.friend_code_draft.clone();
    let name = if state.friend_name_draft.trim().is_empty() {
        code.clone()
    } else {
        state.friend_name_draft.clone()
    };
    match state.store.contacts.add(&code, &name, &own) {
        Ok(()) => {
            let _ = state.store.persist_contacts();
            state.friend_code_draft.clear();
            state.friend_name_draft.clear();
            state.friend_error = None;
        }
        Err(error) => state.friend_error = Some(error.to_string()),
    }
}

/// The centered, accent-bordered monospace plate that shows one's own code.
fn code_plate(ui: &mut egui::Ui, pal: &Palette, code: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 54.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(10),
        pal.window,
        Stroke::new(1.0_f32, pal.accent_dim.gamma_multiply(2.0)),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        code,
        egui::FontId::new(20.0, crate::theme::mono()),
        pal.text_bright,
    );
}

/// Renders one pending invite; returns true if its Cancel was clicked.
fn pending_invite(ui: &mut egui::Ui, pal: &Palette, name: &str, meta: &str) -> bool {
    card(ui, pal.card_alt, pal.border, Margin::symmetric(16, 13), |ui| {
        ui.horizontal(|ui| {
            avatar(ui, 30.0, &crate::ui::initials(name), pal.raised, pal.text);
            ui.add_space(14.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 3.0);
                ui.label(strong(name, 13.0, pal.text_bright));
                ui.label(mono_text(meta, 10.5, pal.text_dim));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                crate::ui::outline_button(ui, pal, "Cancel", 30.0).clicked()
            })
            .inner
        })
        .inner
    })
}


// --- Settings ---

pub fn settings(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    scrolled_page(ui, pal, |ui| {
        ui.label(heading("Settings", 30.0, pal.text_bright));
        ui.add_space(26.0);
        constrained(ui, 760.0, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 16.0);
            identity_card(ui, pal, state);
            capture_card(ui, pal, state);
            codec_card(ui, pal, state);
            network_card(ui, pal, state);
        });
    });
}

fn identity_card(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    settings_card(ui, pal, "Identity", |ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        let mut changed = false;
        ui.columns(2, |cols| {
            changed |= labeled_edit(&mut cols[0], pal, "Display name", &mut state.name_draft);
            changed |= labeled_edit(&mut cols[1], pal, "This device", &mut state.device_draft);
        });
        if changed {
            commit_identity_names(state);
        }
        ui.add_space(14.0);
        ui.label(text(
            "Your key never leaves this machine. Deleting the app deletes the identity — \
             friends will need your new code.",
            12.0,
            pal.text_dim,
        ));
    });
}

fn capture_card(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    let profile = state.store.settings.capture_profile;
    let max = state.store.settings.max_capture.clone();
    settings_card(ui, pal, "Capture defaults", |ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        let mut next_profile = profile;
        let mut next_max = max.clone();
        ui.columns(2, |cols| {
            use clarity_identity::CaptureProfile::{Motion, Text};
            let profiles = [Text.label(), Motion.label()];
            let psel = usize::from(profile == Motion);
            if let Some(i) =
                labeled_dropdown(&mut cols[0], pal, "Profile", "settings-profile", &profiles, psel)
            {
                next_profile = if i == 0 { Text } else { Motion };
            }
            let maxes = ["2560 × 1440", "1920 × 1080"];
            let msel = usize::from(!max.starts_with("2560"));
            if let Some(i) =
                labeled_dropdown(&mut cols[1], pal, "Max capture", "settings-max", &maxes, msel)
            {
                next_max = maxes[i].to_owned();
            }
        });
        if next_profile != profile || next_max != max {
            state.store.settings.capture_profile = next_profile;
            state.store.settings.max_capture = next_max;
            let _ = state.store.persist_settings();
        }
        ui.add_space(16.0);
        let audio = state.store.settings.include_system_audio;
        if checkbox_row(
            ui,
            pal,
            audio,
            "Include system audio when the source allows it",
            "Clarity never asks for your microphone or camera.",
        ) {
            state.store.settings.include_system_audio = !audio;
            let _ = state.store.persist_settings();
        }
    });
}

fn network_card(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    settings_card(ui, pal, "Network", |ui| {
        field_label(ui, pal, "Signaling server");
        ui.add_space(7.0);
        if crate::ui::text_field(
            ui,
            pal,
            &mut state.store.settings.signaling_server,
            "https://clarity.example",
            40.0,
            12.5,
            true,
        )
        .changed()
        {
            state.server_draft = state.store.settings.signaling_server.clone();
            let _ = state.store.persist_settings();
        }
        ui.add_space(14.0);
        connection_banner(ui, pal, state.presence_view.connected);
        ui.add_space(14.0);
        let relay = state.store.settings.always_relay;
        if checkbox_row(
            ui,
            pal,
            relay,
            "Always relay through my server",
            "Hides your IP from peers. Adds latency and uses your server's bandwidth.",
        ) {
            state.store.settings.always_relay = !relay;
            let _ = state.store.persist_settings();
        }
    });
}

/// Writes the drafted display/device names back into the identity and persists.
fn commit_identity_names(state: &mut AppState) {
    let name = state.name_draft.trim().to_owned();
    let device = state.device_draft.trim().to_owned();
    if let Some(identity) = state.store.identity.as_mut() {
        identity.set_display_name(name);
        identity.set_device_name(device);
    }
    let _ = state.store.persist_identity();
}

/// A settings section: an uppercase mono label over its content, in a card.
/// The ranked codec list: every codec the engine knows, in the user's order,
/// each labelled hardware or software so the ranking is an informed choice.
/// Reordering persists the full explicit ranking.
fn codec_card(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    use clarity_client::{VideoCodecId, video_codec_inventory};
    settings_card(ui, pal, "Codec preference", |ui| {
        let inventory = video_codec_inventory();
        // Effective order: the persisted ranking's known ids, then anything
        // it does not mention in the engine's default order — a codec added
        // by a newer build appears rather than vanishing.
        let mut order: Vec<VideoCodecId> = Vec::new();
        for id in state
            .store
            .settings
            .codec_ranking
            .iter()
            .filter_map(|id| VideoCodecId::parse(id))
            .chain(VideoCodecId::ALL)
        {
            if !order.contains(&id) {
                order.push(id);
            }
        }
        let mut moved: Option<(usize, usize)> = None;
        let last = order.len() - 1;
        for (index, codec) in order.iter().enumerate() {
            let capability = inventory.iter().find(|entry| entry.codec == *codec);
            let hardware = capability.is_some_and(|entry| entry.hardware);
            let available = capability.is_some_and(|entry| entry.available);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.add_sized(
                    vec2(14.0, 20.0),
                    egui::Label::new(mono_text(format!("{}", index + 1), 10.0, pal.text_faint)),
                );
                let name_color = if available { pal.text_bright } else { pal.text_dim };
                ui.add_sized(
                    vec2(44.0, 20.0),
                    egui::Label::new(strong(codec.label(), 12.5, name_color)),
                );
                let (badge, badge_color) = if hardware {
                    ("hardware", pal.accent_text)
                } else {
                    ("software", pal.text_muted)
                };
                Frame::new()
                    .fill(if hardware { pal.accent_wash } else { pal.raised })
                    .corner_radius(CornerRadius::same(99))
                    .inner_margin(Margin::symmetric(7, 2))
                    .show(ui, |ui| {
                        ui.label(mono_text(badge, 9.0, badge_color));
                    });
                if !available {
                    ui.label(mono_text("not on this machine", 9.5, pal.text_faint));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.add_enabled_ui(index < last, |ui| {
                        if rank_button(ui, pal, false) {
                            moved = Some((index, index + 1));
                        }
                    });
                    ui.add_enabled_ui(index > 0, |ui| {
                        if rank_button(ui, pal, true) {
                            moved = Some((index, index - 1));
                        }
                    });
                });
            });
            if index < last {
                ui.add_space(4.0);
            }
        }
        if let Some((from, to)) = moved {
            order.swap(from, to);
            state.store.settings.codec_ranking =
                order.iter().map(|id| id.id().to_owned()).collect();
            let _ = state.store.persist_settings();
        }
        ui.add_space(14.0);
        ui.label(text(
            "Every codec this machine can encode is offered, best first; a viewer takes the \
             first it can decode. Hardware encoding is effectively free — software encoding \
             costs CPU at high resolutions.",
            12.0,
            pal.text_dim,
        ));
    });
}

/// A small square rank-reorder button; the arrow is painted (the bundled
/// fonts carry no triangle glyphs), pointing up when `up`.
fn rank_button(ui: &mut egui::Ui, pal: &Palette, up: bool) -> bool {
    let enabled = ui.is_enabled();
    let color = if enabled { pal.text_muted } else { pal.text_faint };
    let response = ui.add_sized(
        vec2(24.0, 22.0),
        egui::Button::new("")
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, pal.border)),
    );
    let center = response.rect.center();
    let (w, h) = (8.0, 4.5);
    let half = vec2(w / 2.0, h / 2.0);
    let points = if up {
        vec![
            center + vec2(-half.x, half.y),
            center + vec2(half.x, half.y),
            center + vec2(0.0, -half.y),
        ]
    } else {
        vec![
            center + vec2(-half.x, -half.y),
            center + vec2(half.x, -half.y),
            center + vec2(0.0, half.y),
        ]
    };
    ui.painter()
        .add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
    if response.hovered() {
        ui.painter().rect_stroke(
            response.rect,
            4.0,
            Stroke::new(1.0, pal.border_strong),
            egui::StrokeKind::Inside,
        );
    }
    response.clicked()
}

fn settings_card(ui: &mut egui::Ui, pal: &Palette, label: &str, body: impl FnOnce(&mut egui::Ui)) {
    card(ui, pal.card, pal.border, Margin::same(22), |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        section_label(ui, pal, label, None);
        ui.add_space(16.0);
        body(ui);
    });
}

/// A labelled editable field; returns true while its text is changing.
fn labeled_edit(ui: &mut egui::Ui, pal: &Palette, label: &str, buffer: &mut String) -> bool {
    field_label(ui, pal, label);
    ui.add_space(7.0);
    crate::ui::text_field(ui, pal, buffer, "", 40.0, 13.0, false).changed()
}

/// A status banner reflecting the live presence connection to the server: green
/// when connected, a neutral amber note otherwise. Honest about real state
/// rather than asserting a fixed "reachable".
fn connection_banner(ui: &mut egui::Ui, pal: &Palette, connected: bool) {
    let (dot, msg) = if connected {
        (pal.green, "Connected to your server.")
    } else {
        (pal.amber, "Not connected yet. Clarity connects when you go online.")
    };
    Frame::NONE
        .fill(dot.gamma_multiply(0.14))
        .stroke(Stroke::new(1.0_f32, dot.gamma_multiply(0.4)))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(13, 11))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                crate::ui::status_dot(ui, 6.0, dot);
                ui.add_space(9.0);
                ui.label(text(msg, 12.0, dot.gamma_multiply(1.15)));
            });
        });
}

// --- Onboarding ---

pub fn onboarding(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    let avail = ui.available_rect_before_wrap();
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(Rect::from_center_size(avail.center(), vec2(520.0, avail.height())))
            .layout(Layout::top_down(Align::Min)),
        |ui| {
            ui.add_space((avail.height() - 600.0).max(0.0) / 2.0);
            crate::chrome::logo_mark(ui, pal, 34.0);
            ui.add_space(24.0);
            ui.label(mono_text("FIRST RUN", 10.0, pal.accent_text.gamma_multiply(0.85)));
            ui.add_space(14.0);
            ui.label(heading("Nothing to sign up for.", 40.0, pal.text_bright));
            ui.add_space(18.0);
            ui.label(text(
                "Clarity makes a key pair on this device and calls that your identity. \
                 Pick a name your friends will recognise — you can change it any time.",
                14.5,
                pal.text_muted,
            ));
            ui.add_space(30.0);
            field_label(ui, pal, "Display name");
            ui.add_space(8.0);
            let submit = crate::ui::text_field(ui, pal, &mut state.name_draft, "Jamie", 48.0, 15.0, false)
                .lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(16.0);
            field_label(ui, pal, "Clarity server");
            ui.add_space(8.0);
            crate::ui::text_field(
                ui,
                pal,
                &mut state.server_draft,
                "https://clarity.example",
                44.0,
                13.0,
                true,
            );
            ui.add_space(6.0);
            ui.label(text(
                "The server that hands out rooms and connects you to friends. \
                 You can change it later in Settings.",
                11.5,
                pal.text_dim,
            ));
            ui.add_space(16.0);
            let clicked =
                accent_button_wide(ui, pal, ui.available_width(), "Create my identity", 48.0)
                    .clicked();
            if (clicked || submit) && !state.name_draft.trim().is_empty() {
                create_identity(state);
            }
            ui.add_space(28.0);
            let (line, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter()
                .rect_filled(line, CornerRadius::ZERO, pal.border_soft);
            ui.add_space(20.0);
            for (n, point) in [
                ("01", "No account, no email, no password."),
                ("02", "Screens and chat go peer to peer and are never stored."),
                ("03", "You add friends by trading a short code."),
            ] {
                ui.horizontal(|ui| {
                    ui.label(mono_text(n, 12.0, pal.accent_text));
                    ui.add_space(10.0);
                    ui.label(text(point, 12.5, pal.text_muted));
                });
                ui.add_space(9.0);
            }
        },
    );
}

/// Creates and persists the identity from the onboarding draft, then goes home.
fn create_identity(state: &mut AppState) {
    let name = state.name_draft.trim().to_owned();
    let device = if state.device_draft.trim().is_empty() {
        "This device".to_owned()
    } else {
        state.device_draft.trim().to_owned()
    };
    let server = state.server_draft.trim();
    if !server.is_empty() {
        state.store.settings.signaling_server = server.to_owned();
        let _ = state.store.persist_settings();
    }
    state.server_draft = state.store.settings.signaling_server.clone();
    match clarity_identity::Identity::create(name, device) {
        Ok(identity) => {
            state.store.identity = Some(identity);
            let _ = state.store.persist_identity();
            state.go(Screen::Home);
        }
        Err(error) => state.friend_error = Some(error.to_string()),
    }
}

// --- Shared page scaffolding ---

/// A vertically scrolling screen body with the design's outer padding.
fn scrolled_page(ui: &mut egui::Ui, _pal: &Palette, body: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::NONE
                .inner_margin(Margin {
                    left: 40,
                    right: 40,
                    top: 38,
                    bottom: 48,
                })
                .show(ui, body);
        });
}

/// Runs `body` in a left-aligned column capped at `width` (or the available
/// width, whichever is smaller), the design's max-width content measure.
fn constrained<R>(ui: &mut egui::Ui, width: f32, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let w = width.min(ui.available_width());
    ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::top_down(Align::Min), body)
        .inner
}

fn accent_button_wide(
    ui: &mut egui::Ui,
    pal: &Palette,
    w: f32,
    label: &str,
    h: f32,
) -> egui::Response {
    wide_button(ui, w, h, label, pal.on_accent, pal.accent, pal.accent)
}

fn neutral_button_wide(ui: &mut egui::Ui, pal: &Palette, w: f32, label: &str) -> egui::Response {
    wide_button(ui, w, 36.0, label, pal.text, pal.raised, pal.border_strong)
}

fn outline_button_wide(ui: &mut egui::Ui, pal: &Palette, w: f32, label: &str) -> egui::Response {
    wide_button(
        ui,
        w,
        36.0,
        label,
        pal.text_muted,
        Color32::TRANSPARENT,
        pal.border,
    )
}

/// A button that fills a given width, for form actions that span a column.
fn wide_button(
    ui: &mut egui::Ui,
    w: f32,
    h: f32,
    label: &str,
    fg: Color32,
    fill: Color32,
    border: Color32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let bg = if resp.hovered() && fill != Color32::TRANSPARENT {
        crate::ui::lighten(fill, 0.06)
    } else if resp.hovered() {
        Color32::from_white_alpha(14)
    } else {
        fill
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        bg,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::new(12.5, crate::theme::medium()),
        fg,
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::palette_match;

    #[test]
    fn palette_match_accepts_substrings_and_subsequences() {
        assert!(palette_match("", "Create a room"));
        assert!(palette_match("room", "Create a room"));
        assert!(palette_match("ROOM", "Create a room"));
        assert!(palette_match("crm", "Create a room"));
        assert!(palette_match("mara", "Join Mara's room"));
        assert!(!palette_match("settings", "Create a room"));
        assert!(!palette_match("zzz", "Join Mara's room"));
    }
}
