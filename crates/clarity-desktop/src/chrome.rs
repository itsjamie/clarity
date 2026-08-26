//! The window chrome: the custom title bar, the left sidebar, and the window
//! controls the OS would normally draw (caption buttons, resize grips). The
//! title bar hides in fullscreen, the sidebar in theatre.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, Pos2, Rect, Sense, Stroke, Vec2,
    vec2,
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
                .inner_margin(Margin {
                    left: 14,
                    right: 8,
                    top: 0,
                    bottom: 0,
                })
                .stroke(Stroke::NONE),
        )
        .show(ui, |ui| {
            let ctx = ui.ctx().clone();
            // The whole bar is a drag handle for moving the window. Start the OS
            // move once, on drag start — sending it every dragged() frame
            // re-grabs the window each frame so it never releases on mouse-up.
            // `click_and_drag` makes egui wait for movement before calling it a
            // drag, which is what keeps double-click distinguishable. Compose
            // the raw senses so this decorative handle stays out of keyboard
            // focus traversal.
            let bar = ui.max_rect();
            let resp = ui.interact(bar, ui.id().with("drag"), title_bar_sense());
            if resp.drag_started() {
                hand_to_compositor(&ctx, egui::ViewportCommand::StartDrag);
            }
            if resp.double_clicked() {
                toggle_maximized(&ctx);
            }
            resp.context_menu(|ui| window_menu(ui, &ctx));

            ui.horizontal_centered(|ui| {
                ui.label(mono_text(window_title(state), 11.0, pal.text_dim));

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let focused = is_focused(&ctx);
                    if caption_tile(ui, pal, Caption::Close, focused).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if caption_tile(ui, pal, Caption::Maximize, focused).clicked() {
                        toggle_maximized(&ctx);
                    }
                    if caption_tile(ui, pal, Caption::Minimize, focused).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                    ui.add_space(10.0);
                    if search_button(ui, pal).clicked() {
                        state.palette_open = true;
                    }
                });
            });
        });
}

fn title_bar_sense() -> Sense {
    Sense::CLICK | Sense::DRAG
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decorative_title_bar_handle_is_not_keyboard_focusable() {
        let sense = title_bar_sense();
        assert!(sense.senses_click());
        assert!(sense.senses_drag());
        assert!(!sense.is_focusable());
    }
}

/// The right-click menu on the title bar: what the OS menu would offer if it
/// were drawing the decorations.
fn window_menu(ui: &mut egui::Ui, ctx: &egui::Context) {
    use egui::ViewportCommand;
    ui.set_min_width(150.0);
    if ui.button("Minimize").clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        ui.close();
    }
    let label = if is_maximized(ctx) { "Restore" } else { "Maximize" };
    if ui.button(label).clicked() {
        toggle_maximized(ctx);
        ui.close();
    }
    let label = if is_fullscreen(ctx) {
        "Exit full screen"
    } else {
        "Full screen"
    };
    if ui.button(format!("{label}    {}", crate::theme::fullscreen_key())).clicked() {
        toggle_fullscreen(ctx);
        ui.close();
    }
    ui.separator();
    if ui.button(format!("Close    {} Q", crate::theme::mod_key())).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Close);
        ui.close();
    }
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Caption {
    Minimize,
    Maximize,
    Close,
}

/// A Windows-style caption button: a flat tile with a 1px glyph that lights
/// up on hover (close goes red, as everywhere else). Glyphs dim with the rest
/// of the chrome when the window loses focus.
fn caption_tile(ui: &mut egui::Ui, pal: &Palette, kind: Caption, focused: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(36.0, 26.0), Sense::click());
    let accessible_label = match kind {
        Caption::Minimize => "Minimize",
        Caption::Maximize if is_maximized(ui.ctx()) => "Restore",
        Caption::Maximize => "Maximize",
        Caption::Close => "Close",
    };
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            accessible_label,
        )
    });
    let hovered = resp.hovered();
    let (bg, fg) = match (kind, hovered, focused) {
        (Caption::Close, true, _) => (Color32::from_rgb(0xc4, 0x2b, 0x1c), Color32::WHITE),
        (_, true, _) => (Color32::from_white_alpha(14), pal.text),
        (_, false, true) => (Color32::TRANSPARENT, pal.text_muted),
        (_, false, false) => (Color32::TRANSPARENT, pal.text_dim),
    };
    let p = ui.painter();
    if hovered {
        p.rect_filled(rect, CornerRadius::same(4), bg);
    }
    if resp.has_focus() {
        p.rect_stroke(
            rect.shrink(1.0),
            CornerRadius::same(4),
            Stroke::new(1.0_f32, pal.accent),
            egui::StrokeKind::Inside,
        );
    }
    // Snap the glyph centre to a pixel centre so 1px strokes stay crisp.
    let ppp = ui.ctx().pixels_per_point();
    let snap = |v: f32| ((v * ppp).floor() + 0.5) / ppp;
    let c = Pos2::new(snap(rect.center().x), snap(rect.center().y));
    let stroke = Stroke::new(1.0_f32, fg);
    match kind {
        Caption::Minimize => {
            p.line_segment([c - vec2(5.0, 0.0), c + vec2(5.0, 0.0)], stroke);
        }
        Caption::Maximize if is_maximized(ui.ctx()) => {
            // Restore: a square with a second one peeking out behind it.
            p.rect_stroke(
                Rect::from_min_size(c + vec2(-5.0, -3.0), Vec2::splat(8.0)),
                CornerRadius::ZERO,
                stroke,
                egui::StrokeKind::Middle,
            );
            p.line(
                vec![c + vec2(-3.0, -5.0), c + vec2(5.0, -5.0), c + vec2(5.0, 3.0)],
                stroke,
            );
        }
        Caption::Maximize => {
            p.rect_stroke(
                Rect::from_center_size(c, Vec2::splat(10.0)),
                CornerRadius::ZERO,
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        Caption::Close => {
            p.line_segment([c - vec2(5.0, 5.0), c + vec2(5.0, 5.0)], stroke);
            p.line_segment([c - vec2(5.0, -5.0), c + vec2(5.0, -5.0)], stroke);
        }
    }
    resp
}

// Window state, read from the viewport info egui is handed every frame.

pub fn is_maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized == Some(true))
}

pub fn is_fullscreen(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().fullscreen == Some(true))
}

/// Unknown counts as focused, so the chrome never starts out dimmed.
fn is_focused(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().focused != Some(false))
}

pub fn toggle_maximized(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized(ctx)));
}

pub fn toggle_fullscreen(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!is_fullscreen(ctx)));
}

/// Starts a compositor-driven move or resize. The compositor grabs the pointer
/// for the rest of the gesture, so egui never sees the button release: it
/// would keep the widget dragged (and its cursor showing) until the next
/// click. Ending the drag and forgetting the pointer here hands it over clean;
/// the next motion event repopulates it.
fn hand_to_compositor(ctx: &egui::Context, cmd: egui::ViewportCommand) {
    ctx.send_viewport_cmd(cmd);
    ctx.stop_dragging();
    ctx.input_mut(|i| i.pointer = Default::default());
}

/// The window body's corner rounding: round while floating, square when it
/// fills the screen.
pub fn body_radius(ctx: &egui::Context) -> u8 {
    if is_maximized(ctx) || is_fullscreen(ctx) { 0 } else { 12 }
}

/// Invisible grips along the window edges and corners. With OS decorations
/// off there is no border to grab, so these hand the drag to the compositor
/// with `BeginResize`; it sizes the window natively and honours the minimum
/// size set at startup. Drawn last, on the foreground layer, so the outer few
/// pixels win over whatever panel content sits under them.
pub fn resize_grips(ctx: &egui::Context) {
    use egui::{CursorIcon, ResizeDirection as Dir, ViewportCommand};

    // winit cannot initiate resize drags on macOS. Its undecorated window keeps
    // the native Borderless + Resizable style, so leave the edge hit-testing
    // to AppKit instead of covering it with grips that cannot hand off.
    if cfg!(target_os = "macos") {
        return;
    }
    if is_maximized(ctx) || is_fullscreen(ctx) {
        return;
    }
    let r = ctx.content_rect();
    let edge = 6.0;
    let corner = 16.0;
    let (l, t, w, h) = (r.left(), r.top(), r.width(), r.height());
    // Edges stop short of the corners so the two never overlap.
    let grips = [
        (
            Rect::from_min_size(Pos2::new(l + corner, t), vec2(w - 2.0 * corner, edge)),
            Dir::North,
            CursorIcon::ResizeNorth,
        ),
        (
            Rect::from_min_size(
                Pos2::new(l + corner, r.bottom() - edge),
                vec2(w - 2.0 * corner, edge),
            ),
            Dir::South,
            CursorIcon::ResizeSouth,
        ),
        (
            Rect::from_min_size(Pos2::new(l, t + corner), vec2(edge, h - 2.0 * corner)),
            Dir::West,
            CursorIcon::ResizeWest,
        ),
        (
            Rect::from_min_size(
                Pos2::new(r.right() - edge, t + corner),
                vec2(edge, h - 2.0 * corner),
            ),
            Dir::East,
            CursorIcon::ResizeEast,
        ),
        (
            Rect::from_min_size(r.left_top(), Vec2::splat(corner)),
            Dir::NorthWest,
            CursorIcon::ResizeNorthWest,
        ),
        (
            Rect::from_min_size(r.right_top() - vec2(corner, 0.0), Vec2::splat(corner)),
            Dir::NorthEast,
            CursorIcon::ResizeNorthEast,
        ),
        (
            Rect::from_min_size(r.left_bottom() - vec2(0.0, corner), Vec2::splat(corner)),
            Dir::SouthWest,
            CursorIcon::ResizeSouthWest,
        ),
        (
            Rect::from_min_size(r.right_bottom() - Vec2::splat(corner), Vec2::splat(corner)),
            Dir::SouthEast,
            CursorIcon::ResizeSouthEast,
        ),
    ];

    egui::Area::new(egui::Id::new("window-resize"))
        .order(egui::Order::Foreground)
        .fixed_pos(Pos2::ZERO)
        .movable(false)
        .show(ctx, |ui| {
            for (i, (rect, dir, cursor)) in grips.iter().enumerate() {
                // Pointer-only: the focusable drag sense would add eight
                // invisible, non-actionable stops to keyboard traversal.
                let resp = ui.interact(*rect, ui.id().with(i), Sense::DRAG);
                if resp.hovered() {
                    ctx.set_cursor_icon(*cursor);
                }
                if resp.drag_started() {
                    hand_to_compositor(ctx, ViewportCommand::BeginResize(*dir));
                }
            }
        });
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
