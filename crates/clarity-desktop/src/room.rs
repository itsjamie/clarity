//! The Room screen. It shows whichever real session is live: the presenter's
//! own room (its link, sharing state, and who is watching) when this app hosts
//! it, or the viewer's stage when watching a friend. With no session it shows
//! an honest empty state rather than fabricated content.
//!
//! The room outlives sharing: a hosted room opens idle, "Share my screen" goes
//! live, "Stop sharing" returns to idle with chat and connections intact, and
//! only "Close room" ends it for everyone.
//!
//! Layout is expressed with nested egui panels so each region keeps its own
//! scroll and clip: a right panel for chat (and, for the presenter, a viewers
//! panel), a top bar, and the central stage.

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, Rect,
    Sense, Stroke, UiBuilder, pos2, vec2,
};

use clarity_protocol::SharingState;

use crate::state::{AppState, RoomTab, Screen, StageFit};
use crate::theme::{Palette, medium, mono};
use crate::ui::{
    accent_button, heading, mono_text, neutral_button, paint_hatch, status_dot, strong, text,
};

const PANEL_W: f32 = 272.0;

pub fn view(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    // The countdown in the header ticks once a minute, but a cheap once-a-second
    // repaint keeps it (and status text) honest even when no frames flow.
    if state.presenter_view.active || state.viewer_view.active {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
    }
    // The Room reflects the real session. Hosting shows the presenter's own
    // room; watching shows the viewer stage; neither shows an honest prompt.
    if state.presenter_view.active {
        presenter_room(ui, pal, state);
    } else if state.viewer_view.active {
        viewer_room(ui, pal, state);
    } else {
        empty_room(ui, pal, state);
    }
    // Theatre's floating chat, over whichever stage is shown.
    if state.theatre_active()
        && state.theatre_chat_open
        && (state.presenter_view.active || state.viewer_view.active)
    {
        let ctx = ui.ctx().clone();
        theatre_chat(&ctx, pal, state);
    }
}

/// Shown when the Room screen is open but no session is live: a plain prompt to
/// start or leave, in place of any fabricated room.
fn empty_room(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(pal.stage))
        .show(ui, |ui| {
            let column = Rect::from_center_size(ui.max_rect().center(), vec2(420.0, 220.0));
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(column)
                    .layout(Layout::top_down(Align::Center)),
                |ui| {
                    ui.label(heading("You're not in a room", 22.0, pal.text_bright));
                    ui.add_space(10.0);
                    ui.label(text(
                        "Open a room to share your screen, or join a friend's room from Home.",
                        13.5,
                        pal.text_muted,
                    ));
                    ui.add_space(22.0);
                    if accent_button(ui, pal, "Open a room", 40.0).clicked() {
                        state.open_create();
                    }
                    ui.add_space(8.0);
                    if neutral_button(ui, pal, "Back to home", 34.0).clicked() {
                        state.go(Screen::Home);
                    }
                },
            );
        });
}

/// "CODE · direct · 5 here · 3h 12m left" — the design's header meta line.
fn header_meta(
    code: Option<&str>,
    relay: bool,
    here: Option<usize>,
    countdown: Option<&crate::state::RoomCountdown>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(code) = code {
        parts.push(short_code(code));
    }
    parts.push(if relay { "relay" } else { "direct" }.to_owned());
    if let Some(here) = here {
        parts.push(format!("{here} here"));
    }
    if let Some(countdown) = countdown {
        parts.push(countdown.label());
    }
    parts.join(" · ")
}

// --- Viewer room (watching a friend's live video) ---

fn viewer_room(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    // Theatre gives the stage the full width; the panel's chat moves to the
    // floating window.
    if !state.theatre_active() {
        egui::Panel::right("viewer-panel")
            .exact_size(PANEL_W)
            .resizable(false)
            .frame(Frame::NONE.fill(pal.panel))
            .show(ui, |ui| room_panel(ui, pal, state));
    }

    egui::Panel::top("viewer-topbar")
        .exact_size(46.0)
        .resizable(false)
        .frame(
            Frame::NONE
                .fill(pal.panel)
                .inner_margin(Margin::symmetric(18, 0)),
        )
        .show(ui, |ui| viewer_topbar(ui, pal, state));

    // With native video the stage must stay transparent, so the panel cannot
    // fill itself; `viewer_stage` paints around the video box instead.
    let stage_frame = if state.viewer_view.native_video {
        Frame::NONE
    } else {
        Frame::NONE.fill(pal.stage)
    };
    egui::CentralPanel::default()
        .frame(stage_frame)
        .show(ui, |ui| viewer_stage(ui, pal, state));
}

/// The room's right panel: the design's Chat / Diagnostics tabs over one body.
/// Shared by the presenter and viewer rooms.
fn room_panel(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    let edge = ui.max_rect();
    ui.painter().vline(
        edge.left(),
        edge.y_range(),
        Stroke::new(1.0_f32, pal.border_soft),
    );
    let peers = if state.presenter_view.active {
        state.presenter_view.viewers.len()
    } else {
        usize::from(state.viewer_view.live)
    };

    egui::Panel::top("room-tabs")
        .exact_size(42.0)
        .resizable(false)
        .frame(
            Frame::NONE
                .fill(pal.panel)
                .inner_margin(Margin::symmetric(10, 0)),
        )
        .show(ui, |ui| {
            let sep = ui.max_rect();
            ui.painter().hline(
                sep.x_range(),
                sep.bottom(),
                Stroke::new(1.0_f32, pal.border_soft),
            );
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if tab_button(ui, pal, "Chat", state.room_tab == RoomTab::Chat).clicked() {
                    state.room_tab = RoomTab::Chat;
                }
                if tab_button(
                    ui,
                    pal,
                    "Diagnostics",
                    state.room_tab == RoomTab::Diagnostics,
                )
                .clicked()
                {
                    state.room_tab = RoomTab::Diagnostics;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = if peers == 1 {
                        "1 peer".to_owned()
                    } else {
                        format!("{peers} peers")
                    };
                    ui.label(mono_text(label, 9.5, pal.text_faint));
                });
            });
        });

    match state.room_tab {
        RoomTab::Chat => chat_body(ui, pal, state),
        RoomTab::Diagnostics => diagnostics_body(ui, pal, state),
    }
}

/// The chat tab: a message input pinned to the bottom over a scrolling log.
/// Reads and appends to whichever session is active.
fn chat_body(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    egui::Panel::bottom("room-chat-input")
        .exact_size(64.0)
        .resizable(false)
        .frame(
            Frame::NONE
                .fill(pal.panel)
                .inner_margin(Margin::symmetric(14, 12)),
        )
        .show(ui, |ui| {
            let sep = ui.max_rect();
            ui.painter().hline(
                sep.x_range(),
                sep.top(),
                Stroke::new(1.0_f32, pal.border_soft),
            );
            let edit = egui::TextEdit::singleline(&mut state.chat_draft)
                .hint_text("Message the room")
                .desired_width(f32::INFINITY)
                .margin(Margin::symmetric(12, 10))
                .background_color(pal.input)
                .font(FontId::new(13.0, FontFamily::Proportional));
            let response = ui.add(edit);
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let text = std::mem::take(&mut state.chat_draft);
                state.send_chat(text);
                response.request_focus();
            }
        });

    let messages: Vec<crate::state::ChatMessage> = if state.presenter_view.active {
        state.presenter_view.messages.clone()
    } else {
        state.viewer_view.messages.clone()
    };
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(pal.panel))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    Frame::NONE
                        .inner_margin(Margin::symmetric(14, 14))
                        .show(ui, |ui| {
                            if messages.is_empty() {
                                ui.label(text("No messages yet. Say hello.", 12.0, pal.text_dim));
                            }
                            ui.spacing_mut().item_spacing.y = 12.0;
                            for message in &messages {
                                chat_row(ui, pal, message);
                            }
                        });
                });
        });
}

/// The diagnostics tab: the presenter sees per-viewer transport stats; the
/// viewer sees its own incoming stream. All from real session data.
fn diagnostics_body(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(pal.panel))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Frame::NONE.inner_margin(Margin::same(14)).show(ui, |ui| {
                        if state.presenter_view.active {
                            presenter_diagnostics(ui, pal, state);
                        } else {
                            viewer_diagnostics(ui, pal, state);
                        }
                        export_section(ui, pal, state);
                    });
                });
        });
}

fn presenter_diagnostics(ui: &mut egui::Ui, pal: &Palette, state: &AppState) {
    let viewers: Vec<crate::state::ViewerCard> =
        state.presenter_view.viewers.values().cloned().collect();
    let watching = viewers.iter().filter(|v| v.connected).count();
    let outgoing: u32 = viewers.iter().filter_map(|v| v.bitrate_kbps).sum();
    ui.spacing_mut().item_spacing.x = 10.0;
    ui.columns(2, |cols| {
        stat_tile(&mut cols[0], pal, "Watching", &watching.to_string());
        stat_tile(
            &mut cols[1],
            pal,
            "Outgoing",
            &format!("{:.1} Mb/s", outgoing as f32 / 1000.0),
        );
    });
    ui.add_space(10.0);
    bitrate_sparkline(ui, pal, &state.presenter_view.bitrate_history);
    ui.add_space(16.0);
    ui.label(mono_text("PER VIEWER", 9.5, pal.text_dim));
    ui.add_space(8.0);
    if viewers.is_empty() {
        ui.label(text(
            "No one is watching yet. Copy the link and send it to a friend.",
            12.0,
            pal.text_dim,
        ));
        return;
    }
    ui.spacing_mut().item_spacing.y = 8.0;
    let relay = state.store.settings.always_relay;
    let ceiling = state.presenter_view.target_ceiling_kbps;
    for viewer in &viewers {
        viewer_row(ui, pal, viewer, relay, ceiling);
    }
}

fn viewer_diagnostics(ui: &mut egui::Ui, pal: &Palette, state: &AppState) {
    let view = &state.viewer_view;
    let incoming = view
        .bitrate_kbps
        .map(|k| format!("{:.1} Mb/s", k as f32 / 1000.0))
        .unwrap_or_else(|| "—".to_owned());
    let rtt = view
        .round_trip_ms
        .map(|r| format!("{r:.0} ms"))
        .unwrap_or_else(|| "—".to_owned());
    ui.spacing_mut().item_spacing.x = 10.0;
    ui.columns(2, |cols| {
        stat_tile(&mut cols[0], pal, "Incoming", &incoming);
        stat_tile(&mut cols[1], pal, "Round trip", &rtt);
    });
    ui.add_space(10.0);
    bitrate_sparkline(ui, pal, &view.bitrate_history);
    ui.add_space(16.0);
    ui.label(mono_text("STREAM", 9.5, pal.text_dim));
    ui.add_space(8.0);
    let resolution = view
        .frame_size
        .map(|(w, h)| format!("{w}×{h}"))
        .unwrap_or_else(|| "—".to_owned());
    let fps = view
        .fps
        .map(|f| format!("{f:.0} fps"))
        .unwrap_or_else(|| "—".to_owned());
    let codec = view.codec.clone().unwrap_or_else(|| "—".to_owned());
    let (loss, loss_hot) = loss_percent(view.packets_lost, view.packets_received);
    Frame::NONE
        .fill(pal.input)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(12, 11))
        .show(ui, |ui| {
            ui.columns(2, |cols| {
                stat_pair(&mut cols[0], pal, "resolution", &resolution);
                stat_pair(&mut cols[1], pal, "codec", &codec);
            });
            ui.add_space(10.0);
            ui.columns(2, |cols| {
                stat_pair(&mut cols[0], pal, "fps", &fps);
                stat_pair_colored(
                    &mut cols[1],
                    pal,
                    "loss",
                    &loss,
                    if loss_hot { pal.amber } else { pal.text },
                );
            });
        });
}

/// The design's 60-second bitrate strip: a filled area under an accent line,
/// newest samples at the right edge. Empty history paints just the frame.
fn bitrate_sparkline(ui: &mut egui::Ui, pal: &Palette, history: &crate::state::BitrateHistory) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 88.0), Sense::hover());
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(9),
        pal.page,
        Stroke::new(1.0_f32, pal.border),
        egui::StrokeKind::Inside,
    );
    let samples = history.samples();
    if samples.len() >= 2 {
        let now = std::time::Instant::now();
        let window = crate::state::BitrateHistory::WINDOW.as_secs_f32();
        let peak = samples
            .iter()
            .map(|(_, kbps)| *kbps)
            .max()
            .unwrap_or(0)
            .max(1) as f32;
        let inner = rect.shrink(1.0);
        let points: Vec<egui::Pos2> = samples
            .iter()
            .map(|(at, kbps)| {
                let age = now.saturating_duration_since(*at).as_secs_f32();
                let x = (inner.right() - age / window * inner.width()).max(inner.left());
                // The line tops out at ~80% of the strip so the label row
                // above it stays clear, as in the design.
                let y = inner.bottom() - (*kbps as f32 / peak) * (inner.height() * 0.78);
                pos2(x, y)
            })
            .collect();
        // The gradient area, as one convex trapezoid per segment (egui fills
        // only convex paths), then the line over it.
        let mut clipped = p.clone();
        clipped.set_clip_rect(inner);
        for pair in points.windows(2) {
            clipped.add(egui::Shape::convex_polygon(
                vec![
                    pair[0],
                    pair[1],
                    pos2(pair[1].x, inner.bottom()),
                    pos2(pair[0].x, inner.bottom()),
                ],
                pal.accent.gamma_multiply(0.18),
                Stroke::NONE,
            ));
        }
        clipped.add(egui::Shape::line(points, Stroke::new(1.5_f32, pal.accent)));
    }
    p.text(
        rect.left_top() + vec2(9.0, 7.0),
        Align2::LEFT_TOP,
        "bitrate · 60s",
        FontId::new(9.0, mono()),
        pal.text_dim,
    );
}

/// Packet loss as a percentage of the cumulative packet count; the flag is
/// true when it is high enough to color as a warning.
fn loss_percent(lost: Option<i64>, total: Option<u64>) -> (String, bool) {
    match (lost, total) {
        (Some(lost), Some(total)) if total > 0 => {
            let pct = lost.max(0) as f64 / total as f64 * 100.0;
            (format!("{pct:.1}%"), pct >= 1.0)
        }
        _ => ("—".to_owned(), false),
    }
}

/// The diagnostics footer: writes the shareable JSON report — room codes and
/// transport stats only, no addresses and no secrets — and shows where it went.
fn export_section(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    ui.add_space(12.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    let (fg, border) = if resp.hovered() {
        (pal.text_bright, pal.border_strong)
    } else {
        (pal.text_muted, pal.border)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        if resp.hovered() {
            Color32::from_white_alpha(10)
        } else {
            Color32::TRANSPARENT
        },
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        "Export redacted report",
        FontId::new(11.5, medium()),
        fg,
    );
    if resp
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        state.report_note = Some(match write_report(state) {
            Ok(path) => format!("Saved to {}", path.display()),
            Err(error) => format!("Export failed: {error}"),
        });
    }
    if let Some(note) = &state.report_note {
        ui.add_space(6.0);
        ui.label(mono_text(note, 9.5, pal.text_dim));
    }
}

/// Writes the redacted diagnostics report to the downloads (or home) folder.
/// Redaction is by construction: only fields listed here are emitted, and the
/// viewer URL (whose fragment carries the room secret) is never among them.
fn write_report(state: &AppState) -> Result<std::path::PathBuf, String> {
    let sharing_label = |sharing: SharingState| match sharing {
        SharingState::Idle => "idle",
        SharingState::Live => "live",
        SharingState::Paused => "paused",
    };
    let history = |history: &crate::state::BitrateHistory| {
        history
            .samples()
            .iter()
            .map(|(_, kbps)| *kbps)
            .collect::<Vec<u32>>()
    };
    let report = if state.presenter_view.active {
        let view = &state.presenter_view;
        serde_json::json!({
            "role": "presenter",
            "room": view.room_id.as_deref().map(short_code),
            "sharing": sharing_label(view.sharing),
            "expiresInSeconds": view.countdown.as_ref().map(|c| c.remaining_seconds()),
            "forceRelay": state.store.settings.always_relay,
            "viewers": view.viewers.values().map(|viewer| serde_json::json!({
                "name": viewer.name,
                "connected": viewer.connected,
                "bitrateKbps": viewer.bitrate_kbps,
                "targetKbps": viewer.target_kbps,
                "roundTripMs": viewer.round_trip_ms,
                "packetsLost": viewer.packets_lost,
                "packetsSent": viewer.packets_sent,
                "codec": viewer.codec,
            })).collect::<Vec<_>>(),
            "outgoingKbpsHistory": history(&view.bitrate_history),
        })
    } else {
        let view = &state.viewer_view;
        serde_json::json!({
            "role": "viewer",
            "room": view.room_code.as_deref().map(short_code),
            "sharing": view.sharing.map(sharing_label),
            "connected": view.live,
            "forceRelay": state.store.settings.always_relay,
            "stream": {
                "resolution": view.frame_size.map(|(w, h)| format!("{w}x{h}")),
                "fps": view.fps,
                "codec": view.codec,
                "bitrateKbps": view.bitrate_kbps,
                "roundTripMs": view.round_trip_ms,
                "packetsLost": view.packets_lost,
                "packetsReceived": view.packets_received,
            },
            "incomingKbpsHistory": history(&view.bitrate_history),
        })
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = download_dir().join(format!("clarity-report-{stamp}.json"));
    let body = serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
    std::fs::write(&path, body).map_err(|error| error.to_string())?;
    Ok(path)
}

/// Where exported reports land: `$XDG_DOWNLOAD_DIR` when set, `~/Downloads`
/// when it exists, home otherwise (temp as the last resort).
fn download_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("XDG_DOWNLOAD_DIR") {
        let dir = std::path::PathBuf::from(dir);
        if dir.is_dir() {
            return dir;
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let downloads = std::path::Path::new(&home).join("Downloads");
        if downloads.is_dir() {
            return downloads;
        }
        return std::path::PathBuf::from(home);
    }
    std::env::temp_dir()
}

/// A diagnostics tile: a small uppercase label over a large mono value.
fn stat_tile(ui: &mut egui::Ui, pal: &Palette, label: &str, value: &str) {
    Frame::NONE
        .fill(pal.input)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(12, 11))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 5.0);
            ui.label(mono_text(label.to_uppercase(), 9.0, pal.text_dim));
            ui.label(mono_text(value, 17.0, pal.text_bright));
        });
}

/// A compact label/value pair for the stream card.
fn stat_pair(ui: &mut egui::Ui, pal: &Palette, label: &str, value: &str) {
    stat_pair_colored(ui, pal, label, value, pal.text);
}

/// A label/value pair whose value carries a state color (e.g. amber loss).
fn stat_pair_colored(ui: &mut egui::Ui, pal: &Palette, label: &str, value: &str, color: Color32) {
    ui.spacing_mut().item_spacing = vec2(0.0, 3.0);
    ui.label(text(label.to_uppercase(), 9.0, pal.text_dim));
    ui.label(mono_text(value, 12.0, color));
}

/// A room panel tab button: accent-filled when active.
fn tab_button(ui: &mut egui::Ui, pal: &Palette, label: &str, active: bool) -> egui::Response {
    let (fg, fill) = if active {
        (pal.text_bright, pal.accent_dim)
    } else {
        (pal.text_dim, Color32::TRANSPARENT)
    };
    small_button(ui, pal, label, fg, fill, Color32::TRANSPARENT, false)
}

fn chat_row(ui: &mut egui::Ui, pal: &Palette, message: &crate::state::ChatMessage) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 3.0);
        let (name, color) = if message.own {
            ("You", pal.accent_text)
        } else {
            (message.from.as_str(), pal.text)
        };
        ui.label(strong(name, 12.0, color));
        ui.label(text(&message.text, 13.0, pal.text));
    });
}

fn viewer_topbar(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    let sep = ui.max_rect();
    ui.painter().hline(
        sep.x_range(),
        sep.bottom(),
        Stroke::new(1.0_f32, pal.border_soft),
    );
    let live = state.viewer_view.live;
    let meta = header_meta(
        state.viewer_view.room_code.as_deref(),
        state.store.settings.always_relay,
        None,
        state.viewer_view.countdown.as_ref(),
    );
    ui.horizontal_centered(|ui| {
        status_dot(ui, 6.0, if live { pal.green } else { pal.amber });
        ui.add_space(7.0);
        ui.label(strong("Watching", 13.0, pal.text_bright));
        ui.add_space(12.0);
        ui.label(mono_text(meta, 10.5, pal.text_dim));
        ui.add_space(12.0);
        ui.label(mono_text(&state.viewer_view.status, 10.5, pal.text_faint));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ghost_button(ui, pal, "Leave", true).clicked() {
                state.leave_room();
            }
            if theatre_button(ui, pal, state.theatre).clicked() {
                state.theatre = !state.theatre;
            }
        });
    });
}

fn viewer_stage(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    let area = ui.max_rect();
    let stage = Rect::from_min_max(area.min + vec2(18.0, 18.0), area.max - vec2(18.0, 18.0));
    let painter = ui.painter().clone();

    // What replaces the picture when there is none: the room-level sharing
    // state, or the connection phase while it is unknown.
    let idle_label = match state.viewer_view.sharing {
        Some(SharingState::Idle) => Some("nothing is being shared right now"),
        Some(SharingState::Paused) => Some("share paused"),
        _ => None,
    };

    if state.viewer_view.native_video {
        // Video arrives on a Wayland subsurface below the window, so only the
        // video's own rect may stay unpainted — a full-stage hole would show
        // the desktop through the letterbox bands. The hole is the aspect-fit
        // rect from the stream's reported dimensions; until those arrive (the
        // first stats tick) the stage stays fully painted. The hole doubles as
        // the sink's render rectangle, so the video fills it exactly.
        // No hole unless the stream is live and something is shared: an ended,
        // idle, or paused room must not leave a transparent cutout showing the
        // desktop.
        let hole = state
            .viewer_view
            .showing_video()
            .then_some(state.viewer_view.frame_size)
            .flatten()
            .filter(|(w, h)| *w > 0 && *h > 0)
            .map(|(w, h)| fit(stage, w as f32 / h as f32));
        state.viewer_view.stage_rect = hole;
        match hole {
            Some(hole) => {
                crate::ui::fill_around(&painter, area, hole, pal.stage);
                painter.rect_stroke(
                    hole,
                    CornerRadius::ZERO,
                    Stroke::new(1.0_f32, pal.border),
                    egui::StrokeKind::Outside,
                );
            }
            None => {
                painter.rect_filled(area, CornerRadius::ZERO, pal.stage);
                paint_hatch(&painter, stage, pal.stage, pal.window, 14.0);
                painter.text(
                    stage.center(),
                    Align2::CENTER_CENTER,
                    idle_label.unwrap_or(state.viewer_view.status.as_str()),
                    FontId::new(13.0, mono()),
                    pal.text_dim,
                );
            }
        }
        if hole.is_some() {
            viewer_stats_pill(&painter, pal, state, stage);
        }
        reconnect_banner(ui, pal, state, stage);
        viewer_controls(ui, pal, state, stage);
        return;
    }
    // The texture path punches no hole; the shell reads None as "no punch".
    state.viewer_view.stage_rect = None;

    painter.rect(
        stage,
        CornerRadius::same(10),
        pal.stage.gamma_multiply(0.6),
        Stroke::new(1.0_f32, pal.border),
        egui::StrokeKind::Inside,
    );

    let has_video = state.viewer_view.showing_video()
        && matches!(
            (&state.viewer_view.texture, state.viewer_view.frame_size),
            (Some(_), Some((w, h))) if w > 0 && h > 0
        );
    if has_video {
        let texture = state.viewer_view.texture.clone().expect("checked above");
        let (w, h) = state.viewer_view.frame_size.expect("checked above");
        paint_video(&painter, stage, &texture, w, h, state.stage_fit);
        viewer_stats_pill(&painter, pal, state, stage);
    } else {
        paint_hatch(&painter, stage, pal.stage, pal.window, 14.0);
        painter.text(
            stage.center(),
            Align2::CENTER_CENTER,
            idle_label.unwrap_or(state.viewer_view.status.as_str()),
            FontId::new(13.0, mono()),
            pal.text_dim,
        );
    }
    reconnect_banner(ui, pal, state, stage);
    viewer_controls(ui, pal, state, stage);
}

/// A floating "Reconnecting…" banner near the top of the stage while the peer
/// connection recovers, with a manual retry. The session already restarts ICE
/// automatically; the button just refuses to make the user wait for the next
/// automatic attempt.
fn reconnect_banner(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState, stage: Rect) {
    if !state.viewer_view.reconnecting || !state.viewer_view.live {
        return;
    }
    let banner = Rect::from_center_size(
        pos2(stage.center().x, stage.top() + 52.0),
        vec2(320.0, 52.0),
    );
    ui.painter().rect(
        banner,
        CornerRadius::same(12),
        pal.raised.gamma_multiply(0.97),
        Stroke::new(1.0_f32, pal.amber.gamma_multiply(0.5)),
        egui::StrokeKind::Inside,
    );
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(banner.shrink2(vec2(14.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            status_dot(ui, 6.0, pal.amber);
            ui.add_space(8.0);
            ui.label(mono_text("Reconnecting…", 11.5, pal.text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if bar_button(ui, pal, "Reconnect now").clicked() {
                    state
                        .viewer_commands
                        .push(crate::state::ViewerCommand::RestartIce);
                }
            });
        },
    );
}

/// The diagnostics pill overlaid on live video: resolution, fps, codec, rate,
/// and round-trip time when known.
fn viewer_stats_pill(painter: &egui::Painter, pal: &Palette, state: &AppState, video: Rect) {
    let view = &state.viewer_view;
    let mut parts: Vec<String> = Vec::new();
    if let Some((w, h)) = view.frame_size {
        parts.push(format!("{w}×{h}"));
    }
    if let Some(fps) = view.fps {
        parts.push(format!("{fps:.0} fps"));
    }
    if let Some(codec) = &view.codec {
        parts.push(codec.clone());
    }
    if let Some(kbps) = view.bitrate_kbps {
        parts.push(format!("{kbps} kb/s"));
    }
    if let Some(rtt) = view.round_trip_ms {
        parts.push(format!("{rtt:.0} ms"));
    }
    if parts.is_empty() {
        return;
    }
    stage_pill(
        painter,
        pal,
        video.left_top() + vec2(12.0, 12.0),
        &parts.join(" · "),
        None,
        pal.text,
    );
}

/// The largest rect of the given aspect ratio that fits inside `bounds`,
/// centered (letterbox/pillarbox).
fn fit(bounds: Rect, aspect: f32) -> Rect {
    let bounds_aspect = bounds.width() / bounds.height().max(1.0);
    let size = if aspect > bounds_aspect {
        vec2(bounds.width(), bounds.width() / aspect)
    } else {
        vec2(bounds.height() * aspect, bounds.height())
    };
    Rect::from_center_size(bounds.center(), size)
}

/// Paints `texture` into `video` under the current fit mode: aspect-fit
/// (letterbox), cover (crop to fill), or native 1:1 centered and clipped.
fn paint_video(
    painter: &egui::Painter,
    video: Rect,
    texture: &egui::TextureHandle,
    w: u32,
    h: u32,
    mode: StageFit,
) {
    let aspect = w as f32 / h as f32;
    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    match mode {
        StageFit::Fit => {
            painter.image(texture.id(), fit(video, aspect), full, Color32::WHITE);
        }
        StageFit::Fill => {
            let bounds_aspect = video.width() / video.height().max(1.0);
            let uv = if aspect > bounds_aspect {
                let m = (1.0 - bounds_aspect / aspect) / 2.0;
                Rect::from_min_max(pos2(m, 0.0), pos2(1.0 - m, 1.0))
            } else {
                let m = (1.0 - aspect / bounds_aspect) / 2.0;
                Rect::from_min_max(pos2(0.0, m), pos2(1.0, 1.0 - m))
            };
            painter.image(texture.id(), video, uv, Color32::WHITE);
        }
        StageFit::Native => {
            let dest = Rect::from_center_size(video.center(), vec2(w as f32, h as f32));
            let mut clipped = painter.clone();
            clipped.set_clip_rect(video);
            clipped.image(texture.id(), dest, full, Color32::WHITE);
        }
    }
}

/// Allocates the floating control bar pinned to the bottom of the stage and
/// paints its chrome; `content` lays the controls inside it.
fn control_bar(ui: &mut egui::Ui, pal: &Palette, video: Rect, content: impl FnOnce(&mut egui::Ui)) {
    let bar = Rect::from_min_max(
        pos2(video.left() + 20.0, video.bottom() - 70.0),
        pos2(video.right() - 20.0, video.bottom() - 16.0),
    );
    ui.painter().rect(
        bar,
        CornerRadius::same(14),
        pal.raised.gamma_multiply(0.96),
        Stroke::new(1.0_f32, pal.border_strong),
        egui::StrokeKind::Inside,
    );
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(bar.shrink2(vec2(16.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
            content(ui);
        },
    );
}

/// The design's room-code pill at the left of the control bar: a green dot,
/// the code, and the transport.
fn room_code_pill(ui: &mut egui::Ui, pal: &Palette, code: Option<&str>, relay: bool) {
    let Some(code) = code else {
        return;
    };
    status_dot(ui, 6.0, pal.green);
    ui.add_space(2.0);
    ui.label(mono_text(short_code(code), 11.5, pal.text_muted));
    ui.label(mono_text(
        if relay { "· relay" } else { "· direct" },
        11.5,
        pal.text_dim,
    ));
}

/// The viewer's control bar: fit modes, volume, rename, tabs, fullscreen, and
/// Leave. Mirrors the design's stage bar.
fn viewer_controls(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState, video: Rect) {
    control_bar(ui, pal, video, |ui| {
        room_code_pill(
            ui,
            pal,
            state.viewer_view.room_code.clone().as_deref(),
            state.store.settings.always_relay,
        );
        v_rule(ui, pal);
        for (label, mode) in [
            ("Fit", StageFit::Fit),
            ("Fill", StageFit::Fill),
            ("1:1", StageFit::Native),
        ] {
            if seg_button(ui, pal, label, state.stage_fit == mode).clicked() {
                state.stage_fit = mode;
            }
        }
        v_rule(ui, pal);
        let mut volume_changed = false;
        if seg_button(ui, pal, "Mute", state.viewer_muted).clicked() {
            state.viewer_muted = !state.viewer_muted;
            volume_changed = true;
        }
        let mut volume = state.viewer_volume;
        if volume_slider(ui, pal, &mut volume) {
            state.viewer_volume = volume;
            state.viewer_muted = false;
            volume_changed = true;
        }
        if volume_changed {
            let level = if state.viewer_muted {
                0.0
            } else {
                f64::from(state.viewer_volume)
            };
            state
                .viewer_commands
                .push(crate::state::ViewerCommand::SetVolume(level));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ghost_button(ui, pal, "Leave", true).clicked() {
                state.leave_room();
            }
            if fullscreen_button(ui, pal).clicked() {
                let fullscreen = ui.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            }
            v_rule(ui, pal);
            if seg_button(
                ui,
                pal,
                "Diagnostics",
                state.room_tab == RoomTab::Diagnostics,
            )
            .clicked()
            {
                state.room_tab = RoomTab::Diagnostics;
            }
            chat_button(ui, pal, state);
            name_editor(ui, pal, state);
        });
    });
}

/// The inline "Set name" editor: a ghost button that becomes a small text
/// field; Enter renames this viewer for the room and for chat.
fn name_editor(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    if state.name_edit_open {
        let edit = egui::TextEdit::singleline(&mut state.name_edit_draft)
            .hint_text("Your name")
            .desired_width(120.0)
            .margin(Margin::symmetric(9, 6))
            .background_color(pal.input)
            .font(FontId::new(12.0, FontFamily::Proportional));
        let response = ui.add(edit);
        if response.lost_focus() {
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let name = state.name_edit_draft.trim().to_owned();
                if !name.is_empty() {
                    state
                        .viewer_commands
                        .push(crate::state::ViewerCommand::SetName(name));
                }
            }
            state.name_edit_open = false;
        } else {
            response.request_focus();
        }
    } else if ghost_button(ui, pal, "Set name", false).clicked() {
        state.name_edit_draft = state.own_display_name();
        state.name_edit_open = true;
    }
}

/// A compact volume slider matching the design's 96-pt track. Returns true
/// while the user drags a new value into `value` (0.0..=1.0).
fn volume_slider(ui: &mut egui::Ui, pal: &Palette, value: &mut f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(96.0, 28.0), Sense::click_and_drag());
    let track = Rect::from_center_size(rect.center(), vec2(rect.width() - 12.0, 4.0));
    let mut changed = false;
    if (resp.dragged() || resp.clicked())
        && let Some(pointer) = resp.interact_pointer_pos()
    {
        let next = ((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0);
        if (next - *value).abs() > f32::EPSILON {
            *value = next;
            changed = true;
        }
    }
    let p = ui.painter();
    p.rect_filled(
        track,
        CornerRadius::same(255),
        Color32::from_white_alpha(28),
    );
    let filled = Rect::from_min_max(
        track.left_top(),
        pos2(track.left() + track.width() * *value, track.bottom()),
    );
    p.rect_filled(filled, CornerRadius::same(255), pal.accent);
    let knob = pos2(filled.right(), track.center().y);
    p.circle_filled(knob, 6.5, pal.text_bright);
    p.circle_stroke(knob, 6.5, Stroke::new(2.0_f32, pal.accent));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    changed
}

/// A 30×28 icon button with a painted expand glyph (the fonts carry no ⤢).
fn fullscreen_button(ui: &mut egui::Ui, pal: &Palette) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 28.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), Color32::from_white_alpha(12));
    }
    let color = if resp.hovered() {
        pal.text_bright
    } else {
        pal.text_muted
    };
    let c = rect.center();
    let r = 5.0;
    for (dx, dy) in [(-1.0_f32, -1.0_f32), (1.0, 1.0)] {
        let corner = c + vec2(dx * r, dy * r);
        ui.painter().line_segment(
            [c + vec2(dx * 1.5, dy * 1.5), corner],
            Stroke::new(1.4_f32, color),
        );
        ui.painter().line_segment(
            [corner, corner - vec2(dx * 3.5, 0.0)],
            Stroke::new(1.4_f32, color),
        );
        ui.painter().line_segment(
            [corner, corner - vec2(0.0, dy * 3.5)],
            Stroke::new(1.4_f32, color),
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn seg_button(ui: &mut egui::Ui, pal: &Palette, label: &str, active: bool) -> egui::Response {
    let (fg, fill, border) = if active {
        (
            pal.text_bright,
            pal.accent_dim.gamma_multiply(0.9),
            pal.accent.gamma_multiply(0.5),
        )
    } else {
        (pal.text_muted, Color32::TRANSPARENT, Color32::TRANSPARENT)
    };
    small_button(ui, pal, label, fg, fill, border, false)
}

fn v_rule(ui: &mut egui::Ui, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 26.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, pal.border_strong);
}

// --- Presenter room (this app hosts the room) ---

fn presenter_room(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    if !state.theatre_active() {
        egui::Panel::right("presenter-panel")
            .exact_size(PANEL_W)
            .resizable(false)
            .frame(Frame::NONE.fill(pal.panel))
            .show(ui, |ui| room_panel(ui, pal, state));
    }

    egui::Panel::top("presenter-topbar")
        .exact_size(46.0)
        .resizable(false)
        .frame(
            Frame::NONE
                .fill(pal.panel)
                .inner_margin(Margin::symmetric(18, 0)),
        )
        .show(ui, |ui| presenter_topbar(ui, pal, state));

    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(pal.stage))
        .show(ui, |ui| presenter_stage(ui, pal, state));
}

fn presenter_topbar(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    let sep = ui.max_rect();
    ui.painter().hline(
        sep.x_range(),
        sep.bottom(),
        Stroke::new(1.0_f32, pal.border_soft),
    );
    let view = &state.presenter_view;
    let failed = view.ended;
    let sharing = view.sharing;
    let open = view.open && !view.ended;
    let url = view.viewer_url.clone();
    let meta = header_meta(
        view.room_id.as_deref(),
        state.store.settings.always_relay,
        Some(view.connected_viewers() + 1),
        view.countdown.as_ref(),
    );
    let dot = if failed {
        pal.red
    } else {
        match sharing {
            SharingState::Live => pal.green,
            SharingState::Paused => pal.amber,
            SharingState::Idle => pal.text_faint,
        }
    };
    ui.horizontal_centered(|ui| {
        status_dot(ui, 6.0, dot);
        ui.add_space(7.0);
        ui.label(strong("Your room", 13.0, pal.text_bright));
        ui.add_space(12.0);
        if open {
            ui.label(mono_text(meta, 10.5, pal.text_dim));
            ui.add_space(12.0);
        }
        ui.label(mono_text(
            &state.presenter_view.status,
            10.5,
            pal.text_faint,
        ));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ghost_button(ui, pal, "Leave", false).clicked() {
                state.leave_presenter();
            }
            if theatre_button(ui, pal, state.theatre).clicked() {
                state.theatre = !state.theatre;
            }
            if open && ghost_button(ui, pal, "Close room", true).clicked() {
                state.close_room();
            }
            ui.add_space(8.0);
            if let Some(url) = url {
                if bar_button(ui, pal, "Copy link").clicked() {
                    ui.ctx().copy_text(url);
                }
                ui.add_space(8.0);
            }
            if open
                && sharing == SharingState::Idle
                && bar_button(ui, pal, "Share my screen").clicked()
            {
                state.start_share();
            }
        });
    });
}

fn presenter_stage(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    let area = ui.max_rect();
    let video = Rect::from_min_max(area.min + vec2(18.0, 18.0), area.max - vec2(18.0, 18.0));
    let painter = ui.painter().clone();

    let ended = state.presenter_view.ended;
    let open = state.presenter_view.open;
    let sharing = state.presenter_view.sharing;
    let reconnecting = state.presenter_view.reconnecting;

    // Once frames arrive, the live stage shows the presenter's own capture — a
    // preview of exactly what viewers see — over a dark backdrop. Every other
    // state is the design's hatched placeholder with a centered mono label.
    let previewing = !ended
        && sharing == SharingState::Live
        && match (
            &state.presenter_view.texture,
            state.presenter_view.frame_size,
        ) {
            (Some(texture), Some((w, h))) if w > 0 && h > 0 => {
                painter.rect_filled(video, CornerRadius::same(10), pal.stage.gamma_multiply(0.6));
                paint_video(&painter, video, texture, w, h, state.stage_fit);
                true
            }
            _ => false,
        };
    if !previewing {
        paint_hatch(&painter, video, pal.stage, pal.window, 14.0);
    }
    painter.rect_stroke(
        video,
        CornerRadius::same(10),
        Stroke::new(1.0_f32, pal.border),
        egui::StrokeKind::Inside,
    );

    if ended {
        let headline = if open {
            "The room has ended"
        } else {
            "Couldn't open the room"
        };
        painter.text(
            video.center() - vec2(0.0, 34.0),
            Align2::CENTER_CENTER,
            headline,
            FontId::new(15.0, medium()),
            if open { pal.text } else { pal.red },
        );
        painter.text(
            video.center() - vec2(0.0, 8.0),
            Align2::CENTER_CENTER,
            &state.presenter_view.status,
            FontId::new(11.0, mono()),
            pal.text_dim,
        );
        let column = Rect::from_center_size(video.center() + vec2(0.0, 46.0), vec2(220.0, 40.0));
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(column)
                .layout(Layout::top_down(Align::Center)),
            |ui| {
                if neutral_button(ui, pal, "Back to home", 34.0).clicked() {
                    state.leave_presenter();
                }
            },
        );
        return;
    }

    if !open {
        painter.text(
            video.center() - vec2(0.0, 10.0),
            Align2::CENTER_CENTER,
            "Setting up your room…",
            FontId::new(15.0, medium()),
            pal.text,
        );
        painter.text(
            video.center() + vec2(0.0, 16.0),
            Align2::CENTER_CENTER,
            &state.presenter_view.status,
            FontId::new(11.0, mono()),
            pal.text_dim,
        );
        return;
    }

    match sharing {
        SharingState::Idle => {
            share_affordance(ui, pal, state, video);
        }
        SharingState::Paused => {
            painter.text(
                video.center(),
                Align2::CENTER_CENTER,
                "share paused",
                FontId::new(13.0, mono()),
                pal.text_dim,
            );
        }
        SharingState::Live if !previewing => {
            painter.text(
                video.center(),
                Align2::CENTER_CENTER,
                "waiting for the first frame…",
                FontId::new(12.0, mono()),
                pal.text_dim,
            );
        }
        SharingState::Live => {}
    }

    // Status pills, top-left: state, then the capture resolution when live.
    let (label, color) = match sharing {
        SharingState::Live => ("LIVE", pal.green),
        SharingState::Paused => ("PAUSED", pal.amber),
        SharingState::Idle => ("IDLE", pal.text_dim),
    };
    let y = video.top() + 14.0;
    let mut x = video.left() + 14.0;
    x += stage_pill(&painter, pal, pos2(x, y), label, Some(color), color);
    if previewing && let Some((w, h)) = state.presenter_view.frame_size {
        x += stage_pill(
            &painter,
            pal,
            pos2(x + 7.0, y),
            &format!("{w}×{h}"),
            None,
            pal.text,
        ) + 7.0;
    }
    if reconnecting {
        stage_pill(
            &painter,
            pal,
            pos2(x + 7.0, y),
            "RECONNECTING",
            Some(pal.amber),
            pal.amber,
        );
    }

    presenter_controls(ui, pal, state, video);
    join_requests(ui, pal, state, video);
}

/// The idle stage's centered "+ share your screen" affordance — the design's
/// dashed tile, scaled up as the room's main empty-state action.
fn share_affordance(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState, video: Rect) {
    let tile = Rect::from_center_size(video.center(), vec2(280.0, 120.0));
    let resp = ui.interact(tile, ui.id().with("share-affordance"), Sense::click());
    let (border, color) = if resp.hovered() {
        (pal.accent.gamma_multiply(0.6), pal.accent_text)
    } else {
        (Color32::from_white_alpha(36), pal.text_dim)
    };
    ui.painter().rect(
        tile,
        CornerRadius::same(10),
        Color32::TRANSPARENT,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        tile.center() - vec2(0.0, 12.0),
        Align2::CENTER_CENTER,
        "+ share your screen",
        FontId::new(13.0, mono()),
        color,
    );
    ui.painter().text(
        tile.center() + vec2(0.0, 14.0),
        Align2::CENTER_CENTER,
        "the room stays open either way",
        FontId::new(10.0, mono()),
        pal.text_faint,
    );
    if resp
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        state.start_share();
    }
}

/// The presenter's control bar: source and pause controls on the left, the
/// destructive Close room pinned right.
fn presenter_controls(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState, video: Rect) {
    let sharing = state.presenter_view.sharing;
    control_bar(ui, pal, video, |ui| {
        room_code_pill(
            ui,
            pal,
            state.presenter_view.room_id.clone().as_deref(),
            state.store.settings.always_relay,
        );
        v_rule(ui, pal);
        match sharing {
            SharingState::Idle => {
                if bar_button(ui, pal, "Share my screen").clicked() {
                    state.start_share();
                }
            }
            SharingState::Live => {
                if bar_button(ui, pal, "Change source").clicked() {
                    state.change_source();
                }
                if bar_button(ui, pal, "Pause").clicked() {
                    state.pause_share();
                }
                if ghost_button(ui, pal, "Stop sharing", false).clicked() {
                    state.stop_share();
                }
            }
            SharingState::Paused => {
                if bar_button(ui, pal, "Resume").clicked() {
                    state.resume_share();
                }
                if ghost_button(ui, pal, "Stop sharing", false).clicked() {
                    state.stop_share();
                }
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ghost_button(ui, pal, "Close room", true).clicked() {
                state.close_room();
            }
            v_rule(ui, pal);
            if seg_button(
                ui,
                pal,
                "Diagnostics",
                state.room_tab == RoomTab::Diagnostics,
            )
            .clicked()
            {
                state.room_tab = RoomTab::Diagnostics;
            }
            chat_button(ui, pal, state);
        });
    });
}

/// The control bar's Chat button. Outside theatre it selects the panel's chat
/// tab; in theatre (no panel) it toggles the floating chat window, as in the
/// design.
fn chat_button(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState) {
    let theatre = state.theatre_active();
    let active = if theatre {
        state.theatre_chat_open
    } else {
        state.room_tab == RoomTab::Chat
    };
    if seg_button(ui, pal, "Chat", active).clicked() {
        if theatre {
            state.theatre_chat_open = !state.theatre_chat_open;
        } else {
            state.room_tab = RoomTab::Chat;
        }
    }
}

/// Pending join requests, floated over the stage's top-right corner. Each is a
/// real decision: nothing is admitted until the presenter answers.
fn join_requests(ui: &mut egui::Ui, pal: &Palette, state: &mut AppState, video: Rect) {
    let requests = state.presenter_view.requests.clone();
    if requests.is_empty() {
        return;
    }
    let width = 264.0;
    let row_h = 76.0;
    let height = 34.0 + row_h * requests.len() as f32;
    let panel = Rect::from_min_size(
        pos2(video.right() - width - 14.0, video.top() + 14.0),
        vec2(width, height.min(video.height() - 28.0)),
    );
    ui.painter().rect(
        panel,
        CornerRadius::same(12),
        pal.raised.gamma_multiply(0.97),
        Stroke::new(1.0_f32, pal.accent.gamma_multiply(0.45)),
        egui::StrokeKind::Inside,
    );
    let mut answer: Option<(String, bool)> = None;
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(panel.shrink2(vec2(12.0, 10.0)))
            .layout(Layout::top_down(Align::Min)),
        |ui| {
            ui.label(mono_text("WAITING TO JOIN", 9.5, pal.text_dim));
            ui.add_space(6.0);
            for request in &requests {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = vec2(0.0, 3.0);
                    ui.label(strong(
                        request.name.as_deref().unwrap_or("Someone"),
                        12.5,
                        pal.text_bright,
                    ));
                    let meta = request
                        .friend_code
                        .clone()
                        .unwrap_or_else(|| "wants to join".to_owned());
                    ui.label(mono_text(meta, 9.5, pal.text_dim));
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if bar_button(ui, pal, "Let in").clicked() {
                        answer = Some((request.peer_id.clone(), true));
                    }
                    if ghost_button(ui, pal, "Deny", true).clicked() {
                        answer = Some((request.peer_id.clone(), false));
                    }
                });
                ui.add_space(8.0);
            }
        },
    );
    if let Some((peer_id, admit)) = answer {
        state.answer_request(peer_id, admit);
    }
}

/// One per-peer diagnostics card, per the design: name and transport badge,
/// then rtt / loss % / codec, and a note when the rate controller has this
/// viewer downshifted below the configured ceiling.
fn viewer_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    viewer: &crate::state::ViewerCard,
    relay: bool,
    ceiling_kbps: u32,
) {
    Frame::NONE
        .fill(pal.input)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(12, 11))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                status_dot(ui, 6.0, if viewer.connected { pal.green } else { pal.amber });
                ui.add_space(8.0);
                ui.label(strong(&viewer.name, 12.5, pal.text_bright));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    transport_badge(ui, pal, relay);
                });
            });
            ui.add_space(10.0);
            let rtt = viewer
                .round_trip_ms
                .map(|r| format!("{r:.0} ms"))
                .unwrap_or_else(|| "—".to_owned());
            let (loss, loss_hot) = loss_percent(viewer.packets_lost, viewer.packets_sent);
            let codec = viewer.codec.clone().unwrap_or_else(|| "—".to_owned());
            ui.columns(3, |cols| {
                stat_pair(&mut cols[0], pal, "rtt", &rtt);
                stat_pair_colored(
                    &mut cols[1],
                    pal,
                    "loss",
                    &loss,
                    if loss_hot { pal.amber } else { pal.text },
                );
                stat_pair(&mut cols[2], pal, "codec", &codec);
            });
            // The adaptive-quality note: the controller has pulled this viewer
            // below the encoder ceiling to ride out congestion.
            if ceiling_kbps > 0 && viewer.target_kbps > 0 && viewer.target_kbps < ceiling_kbps {
                ui.add_space(10.0);
                Frame::NONE
                    .fill(pal.raised)
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(Margin::symmetric(9, 8))
                    .show(ui, |ui| {
                        ui.label(text(
                            format!(
                                "Dropped to {:.1} Mb/s to hold frame rate. Recovering when loss clears.",
                                viewer.target_kbps as f32 / 1000.0
                            ),
                            11.0,
                            pal.text_muted,
                        ));
                    });
            }
        });
}

/// The design's direct/relay transport pill: green for a direct path, amber
/// for the TURN relay.
fn transport_badge(ui: &mut egui::Ui, pal: &Palette, relay: bool) {
    let (label, color) = if relay {
        ("relay", pal.amber)
    } else {
        ("direct", pal.green)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::new(9.5, medium()), color);
    let (rect, _) = ui.allocate_exact_size(vec2(galley.size().x + 16.0, 18.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(255),
        color.gamma_multiply(0.16),
        Stroke::new(1.0_f32, color.gamma_multiply(0.45)),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
}

/// A short, display-friendly slice of a room id.
fn short_code(room_id: &str) -> String {
    room_id.chars().take(8).collect::<String>().to_uppercase()
}

/// Paints a rounded status pill at `top_left`, optionally led by a dot; returns
/// the pill's width so callers can lay pills left to right.
fn stage_pill(
    painter: &egui::Painter,
    _pal: &Palette,
    top_left: egui::Pos2,
    label: &str,
    dot: Option<Color32>,
    fg: Color32,
) -> f32 {
    let font = FontId::new(10.0, mono());
    let galley = painter.layout_no_wrap(label.to_owned(), font.clone(), fg);
    let dot_w = if dot.is_some() { 11.0 } else { 0.0 };
    let w = galley.size().x + 20.0 + dot_w;
    let rect = Rect::from_min_size(top_left, vec2(w, 20.0));
    painter.rect_filled(
        rect,
        CornerRadius::same(255),
        Color32::from_black_alpha(210),
    );
    let mut tx = rect.left() + 10.0;
    if let Some(c) = dot {
        painter.circle_filled(pos2(tx + 2.5, rect.center().y), 2.5, c);
        tx += dot_w;
    }
    painter.galley(
        pos2(tx, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
    w
}

// --- Theatre mode ---

/// The top bar's Theatre toggle: accent-bordered while active, with the
/// design's mono "T" key hint.
fn theatre_button(ui: &mut egui::Ui, pal: &Palette, active: bool) -> egui::Response {
    let (fg, hint, fill, border) = if active {
        (
            pal.text_bright,
            pal.text_bright.gamma_multiply(0.6),
            pal.accent_dim,
            pal.accent.gamma_multiply(0.55),
        )
    } else {
        (
            pal.text_muted,
            pal.text_faint,
            Color32::TRANSPARENT,
            pal.border_strong,
        )
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(
        "Theatre ",
        0.0,
        egui::TextFormat::simple(FontId::new(11.5, medium()), fg),
    );
    job.append(
        "T",
        0.0,
        egui::TextFormat::simple(FontId::new(10.0, mono()), hint),
    );
    let galley = ui.painter().layout_job(job);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 22.0, 28.0), Sense::click());
    let bg = if resp.hovered() && !active {
        Color32::from_white_alpha(10)
    } else {
        fill
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(7),
        bg,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The design's floating chat in theatre mode: a 296-pt window with a
/// "Chat · ephemeral" header that drags it around, the message log, and an
/// input. Its position persists in [`AppState::theatre_chat_pos`] until the
/// app closes; the control bar's Chat button toggles it.
fn theatre_chat(ctx: &egui::Context, pal: &Palette, state: &mut AppState) {
    const W: f32 = 296.0;
    let content = ctx.content_rect();
    let default_pos = pos2(content.right() - W - 24.0, content.top() + 96.0);
    let mut pos = state.theatre_chat_pos.unwrap_or(default_pos);
    // Keep the header reachable so the window can always be dragged back.
    pos.x = pos.x.clamp(
        content.left() + 8.0,
        (content.right() - W - 8.0).max(content.left() + 8.0),
    );
    pos.y = pos.y.clamp(
        content.top() + 46.0,
        (content.bottom() - 160.0).max(content.top() + 46.0),
    );
    let messages: Vec<crate::state::ChatMessage> = if state.presenter_view.active {
        state.presenter_view.messages.clone()
    } else {
        state.viewer_view.messages.clone()
    };
    let mut close = false;
    egui::Area::new(egui::Id::new("theatre-chat"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        // No fade-in: the window toggles instantly, and the fade would leave
        // it translucent over the stage while animating.
        .fade_in(false)
        .show(ctx, |ui| {
            Frame::NONE
                .fill(pal.raised)
                .stroke(Stroke::new(1.0_f32, pal.border_strong))
                .corner_radius(CornerRadius::same(12))
                .show(ui, |ui| {
                    ui.set_width(W);
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

                    // The header is the drag handle. The close button is
                    // registered after it, so it wins the pointer over the
                    // drag sense (same pattern as the window title bar).
                    let (header, drag) =
                        ui.allocate_exact_size(vec2(W, 38.0), Sense::click_and_drag());
                    if drag.dragged() {
                        state.theatre_chat_pos = Some(pos + drag.drag_delta());
                    }
                    drag.on_hover_cursor(egui::CursorIcon::Grab);
                    let p = ui.painter();
                    p.hline(
                        header.x_range(),
                        header.bottom(),
                        Stroke::new(1.0_f32, pal.border_soft),
                    );
                    let title =
                        p.layout_no_wrap("Chat".to_owned(), FontId::new(12.0, medium()), pal.text);
                    let title_w = title.size().x;
                    p.galley(
                        pos2(
                            header.left() + 12.0,
                            header.center().y - title.size().y / 2.0,
                        ),
                        title,
                        pal.text,
                    );
                    p.text(
                        pos2(header.left() + 12.0 + title_w + 8.0, header.center().y),
                        Align2::LEFT_CENTER,
                        "ephemeral",
                        FontId::new(9.5, mono()),
                        pal.text_faint,
                    );
                    let close_rect = Rect::from_center_size(
                        pos2(header.right() - 17.0, header.center().y),
                        egui::Vec2::splat(22.0),
                    );
                    let close_resp = ui.interact(close_rect, ui.id().with("close"), Sense::click());
                    if close_resp.hovered() {
                        ui.painter().rect_filled(
                            close_rect,
                            CornerRadius::same(5),
                            Color32::from_white_alpha(14),
                        );
                    }
                    let x_color = if close_resp.hovered() {
                        pal.text_bright
                    } else {
                        pal.text_dim
                    };
                    let c = close_rect.center();
                    for d in [vec2(-3.5, -3.5), vec2(-3.5, 3.5)] {
                        ui.painter()
                            .line_segment([c + d, c - d], Stroke::new(1.3_f32, x_color));
                    }
                    if close_resp
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        close = true;
                    }

                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .auto_shrink([false, true])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            Frame::NONE.inner_margin(Margin::same(12)).show(ui, |ui| {
                                if messages.is_empty() {
                                    ui.label(text(
                                        "No messages yet. Say hello.",
                                        12.0,
                                        pal.text_dim,
                                    ));
                                }
                                ui.spacing_mut().item_spacing.y = 12.0;
                                for message in &messages {
                                    chat_row(ui, pal, message);
                                }
                            });
                        });

                    let (line, _) = ui.allocate_exact_size(vec2(W, 1.0), Sense::hover());
                    ui.painter()
                        .rect_filled(line, CornerRadius::ZERO, pal.border_soft);
                    Frame::NONE
                        .inner_margin(Margin {
                            left: 12,
                            right: 12,
                            top: 10,
                            bottom: 12,
                        })
                        .show(ui, |ui| {
                            let edit = egui::TextEdit::singleline(&mut state.chat_draft)
                                .hint_text("Message the room")
                                .desired_width(f32::INFINITY)
                                .margin(Margin::symmetric(11, 9))
                                .background_color(pal.input)
                                .font(FontId::new(12.5, FontFamily::Proportional));
                            let response = ui.add(edit);
                            if response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                let text = std::mem::take(&mut state.chat_draft);
                                state.send_chat(text);
                                response.request_focus();
                            }
                        });
                });
        });
    if close {
        state.theatre_chat_open = false;
    }
}

// --- Small buttons shared within the room ---

/// A raised bar button, e.g. the room's "Copy link".
fn bar_button(ui: &mut egui::Ui, pal: &Palette, label: &str) -> egui::Response {
    small_button(
        ui,
        pal,
        label,
        pal.text,
        pal.raised,
        pal.border_strong,
        false,
    )
}

fn ghost_button(ui: &mut egui::Ui, pal: &Palette, label: &str, danger: bool) -> egui::Response {
    small_button(
        ui,
        pal,
        label,
        pal.text_muted,
        Color32::TRANSPARENT,
        Color32::TRANSPARENT,
        danger,
    )
}

/// A compact bar/tab button sized to its label. `danger` tints the hover red,
/// for destructive actions like Leave and Close room.
fn small_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    label: &str,
    fg: Color32,
    fill: Color32,
    border: Color32,
    danger: bool,
) -> egui::Response {
    let font = FontId::new(11.5, medium());
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);
    let size = vec2(galley.size().x + 22.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hovered = resp.hovered();
    let bg = if hovered && fill != Color32::TRANSPARENT {
        crate::ui::lighten(fill, 0.06)
    } else if hovered && !danger {
        Color32::from_white_alpha(12)
    } else {
        fill
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(7),
        bg,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    let fg = if danger && hovered { pal.red } else { fg };
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    resp
}

#[cfg(test)]
mod tests {
    use super::loss_percent;

    #[test]
    fn loss_percent_formats_and_flags_high_loss() {
        assert_eq!(loss_percent(None, None).0, "—");
        assert_eq!(loss_percent(Some(5), Some(0)).0, "—");
        assert_eq!(
            loss_percent(Some(0), Some(1000)),
            ("0.0%".to_owned(), false)
        );
        assert_eq!(
            loss_percent(Some(14), Some(1000)),
            ("1.4%".to_owned(), true)
        );
        // A negative cumulative counter (RTCP quirk) clamps to zero.
        assert_eq!(loss_percent(Some(-5), Some(1000)).0, "0.0%");
    }
}
