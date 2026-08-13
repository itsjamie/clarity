//! The window chrome: the custom title bar and the left sidebar. Both persist
//! across every screen (the sidebar hides only in theatre mode).

use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, Sense, Stroke, Vec2, vec2,
};

use crate::state::{AppState, Screen};
use crate::theme::Palette;
use crate::ui::{avatar, mono_text, section_label, status_dot, strong, text};

const SIDEBAR_W: f32 = 212.0;

pub fn title_bar(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    egui::Panel::top("titlebar")
        .exact_size(38.0)
        .frame(
            Frame::NONE
                .fill(pal.panel)
                .inner_margin(Margin::symmetric(14, 0))
                .stroke(Stroke::NONE),
        )
        .show(ui, |ui| {
            // The whole bar is a drag handle for moving the window. Start the OS
            // move once, on drag start — sending it every dragged() frame
            // re-grabs the window each frame so it never releases on mouse-up.
            let bar = ui.max_rect();
            if ui
                .interact(bar, ui.id().with("drag"), Sense::click_and_drag())
                .drag_started()
            {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            ui.horizontal_centered(|ui| {
                traffic_light(ui, pal.red, true);
                traffic_light(ui, pal.amber, false);
                traffic_light(ui, pal.green, false);
                ui.add_space(6.0);
                ui.label(mono_text(window_title(state), 11.0, pal.text_dim));

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if search_button(ui, pal).clicked() {
                        state.palette_open = true;
                    }
                });
            });
        });
}

fn window_title(state: &AppState) -> String {
    let suffix = match state.screen {
        Screen::Home => "Home",
        // In the Room, reflect the real session rather than a fixed name.
        Screen::Room if state.presenter_view.active => "Your room",
        Screen::Room if state.viewer_view.active => "Watching",
        Screen::Room => "Room",
        Screen::Friends => "Add friend",
        Screen::Settings => "Settings",
        Screen::Onboarding => "Welcome",
    };
    format!("Clarity — {suffix}")
}

fn traffic_light(ui: &mut egui::Ui, color: Color32, closes: bool) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(11.0), Sense::click());
    ui.painter().circle_filled(rect.center(), 5.5, color);
    if closes && resp.clicked() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// The full-width "Join by link" button under Create, matching the design's
/// sidebar. Neutral (transparent, bordered) so Create stays the primary action.
fn join_link_button(ui: &mut egui::Ui, pal: &Palette) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    let (fg, bg, border) = if resp.hovered() {
        (pal.text, Color32::from_white_alpha(10), pal.border_strong)
    } else {
        (pal.text_muted, Color32::TRANSPARENT, pal.border)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        bg,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    let galley = ui.painter().layout_no_wrap(
        "Join by link".to_owned(),
        egui::FontId::new(12.0, crate::theme::medium()),
        fg,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn search_button(ui: &mut egui::Ui, pal: &Palette) -> egui::Response {
    let label = format!("Search or run · {} K", crate::theme::mod_key());
    let galley = ui.painter().layout_no_wrap(
        label.clone(),
        egui::FontId::new(10.5, crate::theme::mono()),
        pal.text_dim,
    );
    let size = vec2(galley.size().x + 18.0, 23.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let border = if resp.hovered() {
        pal.border_strong
    } else {
        pal.border
    };
    let fg = if resp.hovered() {
        pal.text
    } else {
        pal.text_dim
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(6),
        pal.card,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    let galley = ui.painter().layout_no_wrap(
        label,
        egui::FontId::new(10.5, crate::theme::mono()),
        fg,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    resp
}

pub fn sidebar(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    egui::Panel::left("sidebar")
        .exact_size(SIDEBAR_W)
        .resizable(false)
        .frame(Frame::NONE.fill(pal.panel))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

            // The identity footer, pinned to the bottom, is the entry to Settings.
            egui::Panel::bottom("sidebar-footer")
                .exact_size(50.0)
                .resizable(false)
                .frame(Frame::NONE.fill(pal.panel))
                .show(ui, |ui| identity_footer(ui, pal, state));

            // Create a room / join by link.
            egui::Panel::top("sidebar-actions")
                .resizable(false)
                .frame(Frame::NONE.fill(pal.panel).inner_margin(Margin {
                    left: 12,
                    right: 12,
                    top: 14,
                    bottom: 10,
                }))
                .show(ui, |ui| {
                    if start_room_button(ui, pal).clicked() {
                        state.open_create();
                    }
                    ui.add_space(6.0);
                    if join_link_button(ui, pal).clicked() {
                        state.open_join();
                    }
                });

            // Live now + friends, scrolling in the middle.
            egui::CentralPanel::default()
                .frame(Frame::NONE.fill(pal.panel))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            Frame::NONE
                                .inner_margin(Margin::symmetric(8, 6))
                                .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing = vec2(0.0, 6.0);
                                    live_now(ui, pal, state);
                                    ui.add_space(4.0);
                                    friends_list(ui, pal, state);
                                });
                        });
                });
        });
}

/// The bottom-of-sidebar identity card: this device's name and friend code, a
/// full-width button into Settings. This is the design's only visible way back
/// to Settings from the main screens.
fn identity_footer(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    let area = ui.max_rect();
    // Hover background and the top hairline, painted before the content.
    if ui.rect_contains_pointer(area) {
        ui.painter()
            .rect_filled(area, CornerRadius::ZERO, Color32::from_white_alpha(8));
    }
    ui.painter()
        .hline(area.x_range(), area.top(), Stroke::new(1.0_f32, pal.border_soft));

    if let Some(identity) = &state.store.identity {
        let name = identity.display_name().to_owned();
        let code = identity.friend_code();
        Frame::NONE
            .inner_margin(Margin::symmetric(14, 11))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    avatar(
                        ui,
                        26.0,
                        &crate::ui::initials(&name),
                        pal.accent.gamma_multiply(0.55),
                        pal.accent_text,
                    );
                    ui.add_space(9.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
                        ui.label(strong(&name, 12.0, pal.text_bright));
                        ui.label(mono_text(&code, 9.5, pal.text_dim));
                    });
                });
            });
    }

    if ui
        .interact(area, ui.id().with("identity-footer"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        state.go(Screen::Settings);
    }
}

fn live_now(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    // Friends hosting a room right now, with their local contact names.
    let live: Vec<(String, String, u32, clarity_protocol::SharingState)> = state
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

    let badge = (!live.is_empty()).then(|| live.len().to_string());
    Frame::NONE
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| section_label(ui, pal, "Live now", badge.as_deref()));
    ui.add_space(4.0);

    if live.is_empty() {
        Frame::NONE
            .inner_margin(Margin::symmetric(9, 6))
            .show(ui, |ui| {
                ui.label(text("No friends are sharing right now.", 11.0, pal.text_dim));
            });
        return;
    }

    for (name, url, viewers, sharing) in live {
        // "Here" counts everyone in the room including the presenter, the
        // same convention as the web sidebar and room panel headers
        // (`viewer_count` itself excludes the presenter).
        let here = viewers.saturating_add(1);
        // The design distinguishes a live share from a room sitting open idle.
        let (dot, label) = match sharing {
            clarity_protocol::SharingState::Live => (pal.green, format!("sharing · {here} here")),
            clarity_protocol::SharingState::Paused => (pal.amber, format!("paused · {here} here")),
            clarity_protocol::SharingState::Idle => {
                (pal.text_dim, format!("idle room · {here} here"))
            }
        };
        crate::ui::card(
            ui,
            pal.accent_wash,
            pal.accent_dim.gamma_multiply(2.4),
            Margin::same(9),
            |ui| {
                ui.horizontal(|ui| {
                    avatar(
                        ui,
                        26.0,
                        &crate::ui::initials(&name),
                        pal.accent.gamma_multiply(0.55),
                        pal.accent_text,
                    );
                    ui.add_space(9.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
                        ui.label(strong(&name, 12.5, pal.text_bright));
                        ui.horizontal(|ui| {
                            status_dot(ui, 5.0, dot);
                            ui.add_space(3.0);
                            ui.label(mono_text(label, 9.5, dot));
                        });
                    });
                });
                ui.add_space(9.0);
                if crate::ui::neutral_button(ui, pal, "Join", 28.0)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    state.join_room(url);
                }
            },
        );
        ui.add_space(6.0);
    }
}

fn friends_list(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    ui.add_space(4.0);
    let count = state.store.contacts.len();
    let badge = (count > 0).then(|| count.to_string());
    Frame::NONE
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| section_label(ui, pal, "Friends", badge.as_deref()));
    ui.add_space(2.0);

    if count == 0 {
        Frame::NONE
            .inner_margin(Margin::symmetric(9, 6))
            .show(ui, |ui| {
                ui.label(text(
                    "No friends yet. Trade a code to add one.",
                    11.0,
                    pal.text_dim,
                ));
            });
    } else {
        let rows: Vec<(String, bool, bool, Option<u64>)> = state
            .store
            .contacts
            .iter()
            .map(|contact| {
                let presence = state.presence_view.friends.get(&contact.code);
                let online = presence.is_some_and(|friend| friend.online);
                let last_seen = presence.and_then(|friend| friend.last_seen_seconds_ago);
                (contact.name.clone(), contact.pending, online, last_seen)
            })
            .collect();
        for (name, pending, online, last_seen) in rows {
            friend_row(ui, pal, &name, pending, online, last_seen);
        }
    }

    ui.add_space(10.0);
    // Incoming requests not yet acted on surface here, so an invite is
    // noticeable without opening the Friends screen.
    let invites = state
        .presence_view
        .requests
        .iter()
        .filter(|code| state.store.contacts.name_of(code).is_none())
        .filter(|code| !state.store.settings.dismissed_requests.contains(code))
        .count();
    if add_friend_button(ui, pal, invites).clicked() {
        state.go(Screen::Friends);
    }
}

fn friend_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    name: &str,
    pending: bool,
    online: bool,
    last_seen: Option<u64>,
) {
    // Offline friends dim, per the design's faded rows with a "2d" label.
    let name_color = if online || pending {
        pal.text
    } else {
        pal.text_muted
    };
    Frame::NONE
        .inner_margin(Margin::symmetric(9, 7))
        .corner_radius(CornerRadius::same(7))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                avatar(ui, 24.0, &crate::ui::initials(name), pal.raised, name_color);
                ui.add_space(9.0);
                ui.label(strong(name, 12.5, name_color));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if pending {
                        ui.label(mono_text("pending", 9.5, pal.amber));
                    } else if online {
                        status_dot(ui, 6.0, pal.green);
                    } else if let Some(seconds) = last_seen {
                        ui.label(mono_text(
                            clarity_core::ago_compact(seconds),
                            9.5,
                            pal.text_faint,
                        ));
                    } else {
                        status_dot(ui, 6.0, pal.text_faint);
                    }
                });
            });
        });
}

pub(crate) fn logo_mark(ui: &mut egui::Ui, pal: &Palette, size: f32) {
    // A compact lighthouse glyph echoing the SVG: a violet beam-tower.
    let (rect, _) = ui.allocate_exact_size(vec2(size * 0.85, size), Sense::hover());
    let p = ui.painter();
    let cx = rect.center().x;
    let top = rect.top();
    let bot = rect.bottom();
    let beam = pal.accent;
    // lamp
    p.circle_filled(egui::pos2(cx, top + size * 0.16), size * 0.16, beam);
    // tower body (trapezoid via two triangles)
    let w_top = size * 0.16;
    let w_bot = size * 0.30;
    let y0 = top + size * 0.34;
    p.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(cx - w_top, y0),
            egui::pos2(cx + w_top, y0),
            egui::pos2(cx + w_bot, bot),
            egui::pos2(cx - w_bot, bot),
        ],
        beam.gamma_multiply(0.9),
        Stroke::NONE,
    ));
}

fn start_room_button(ui: &mut egui::Ui, pal: &Palette) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
    let bg = if resp.hovered() {
        pal.accent.gamma_multiply(0.28)
    } else {
        pal.accent_dim
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        bg,
        Stroke::new(1.0_f32, pal.accent.gamma_multiply(0.5)),
        egui::StrokeKind::Inside,
    );
    let mut job = egui::text::LayoutJob::default();
    job.append(
        "Create room  ",
        0.0,
        egui::TextFormat::simple(
            egui::FontId::new(12.5, crate::theme::medium()),
            pal.accent_text,
        ),
    );
    job.append(
        &format!("{} N", crate::theme::mod_key()),
        0.0,
        egui::TextFormat::simple(
            egui::FontId::new(10.0, crate::theme::mono()),
            pal.accent_text.gamma_multiply(0.7),
        ),
    );
    let galley = ui.painter().layout_job(job);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, pal.accent_text);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn add_friend_button(ui: &mut egui::Ui, pal: &Palette, invites: usize) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let border = if invites > 0 {
        pal.accent.gamma_multiply(0.5)
    } else if resp.hovered() {
        pal.border_strong
    } else {
        pal.border
    };
    let fg = if invites > 0 {
        pal.accent_text
    } else if resp.hovered() {
        pal.text
    } else {
        pal.text_dim
    };
    // Dashed border approximated with a solid subtle stroke.
    ui.painter().rect(
        rect,
        CornerRadius::same(7),
        Color32::TRANSPARENT,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    let label = match invites {
        0 => "Add a friend".to_owned(),
        1 => "Add a friend · 1 invite".to_owned(),
        n => format!("Add a friend · {n} invites"),
    };
    let galley = ui.painter().layout_no_wrap(
        label,
        egui::FontId::new(11.5, egui::FontFamily::Proportional),
        fg,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}
