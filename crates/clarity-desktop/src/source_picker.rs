//! The Windows source picker.
//!
//! Windows has no system dialog for choosing what to share — the role the
//! desktop portal plays on Linux — so "Share my screen" and "Change source"
//! open this list of displays and windows instead. One click shares that
//! source; the scrim, the close button, or Escape dismiss it.

use clarity_client::{CaptureSource, CaptureTarget};
use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, Sense, Stroke, UiBuilder, vec2,
};

use crate::state::AppState;
use crate::theme::Palette;
use crate::ui::{mono_text, section_label, strong, text};

pub fn source_picker_modal(ctx: &egui::Context, pal: &Palette, state: &mut AppState) {
    let Some(sources) = state.source_picker.clone() else {
        return;
    };
    let mut close = false;
    let mut refresh = false;
    let mut chosen: Option<CaptureTarget> = None;

    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new("source-picker-scrim"))
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
                close = true;
            }
        });

    let (displays, windows): (Vec<&CaptureSource>, Vec<&CaptureSource>) = sources
        .iter()
        .partition(|source| matches!(source.target, CaptureTarget::Display(_)));

    egui::Window::new("source-picker")
        .title_bar(false)
        .order(egui::Order::Foreground)
        .resizable(false)
        .fixed_size(vec2(520.0, 0.0))
        .anchor(egui::Align2::CENTER_TOP, vec2(0.0, 72.0))
        .frame(
            Frame::NONE
                .fill(pal.card)
                .stroke(Stroke::new(1.0_f32, pal.border_strong))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(22)),
        )
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 10.0);
            ui.horizontal(|ui| {
                ui.label(mono_text("SHARE YOUR SCREEN", 10.0, pal.text_dim));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if crate::screens::modal_close(ui, pal).clicked() {
                        close = true;
                    }
                    ui.add_space(10.0);
                    let refresh_link = ui
                        .add(
                            egui::Label::new(mono_text("REFRESH", 10.0, pal.accent_text))
                                .sense(Sense::click())
                                .selectable(false),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if refresh_link.clicked() {
                        refresh = true;
                    }
                });
            });

            section_label(ui, pal, "Displays", Some(&displays.len().to_string()));
            for source in &displays {
                if source_row(ui, pal, source).clicked() {
                    chosen = Some(source.target);
                }
            }

            ui.add_space(4.0);
            section_label(ui, pal, "Windows", Some(&windows.len().to_string()));
            if windows.is_empty() {
                ui.label(text("No other windows are open.", 11.5, pal.text_dim));
            } else {
                // The modal sizes to its content, which leaves a scroll area
                // nothing to grow into; without a floor it collapses to a
                // single row. Short lists still shrink to fit.
                egui::ScrollArea::vertical()
                    .min_scrolled_height(320.0)
                    .max_height(320.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = vec2(0.0, 6.0);
                        for source in &windows {
                            if source_row(ui, pal, source).clicked() {
                                chosen = Some(source.target);
                            }
                        }
                    });
            }

            ui.label(text(
                "Viewers see only what you pick. A minimized window sends no picture until you restore it.",
                11.0,
                pal.text_dim,
            ));
        });

    if let Some(target) = chosen {
        state.share_target(target);
    } else if close {
        state.source_picker = None;
    } else if refresh {
        state.open_source_picker();
    }
}

/// One clickable source: its name over a mono detail line, both cut to one
/// line so long window titles cannot stretch the modal.
fn source_row(ui: &mut egui::Ui, pal: &Palette, source: &CaptureSource) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::click());
    let hovered = response.hovered();
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius::same(9),
        if hovered { pal.raised } else { pal.input },
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(9),
        Stroke::new(
            1.0_f32,
            if hovered {
                pal.border_strong
            } else {
                pal.border
            },
        ),
        egui::StrokeKind::Inside,
    );
    let mut content = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(14.0, 8.0)))
            .layout(Layout::top_down(Align::Min)),
    );
    content.spacing_mut().item_spacing = vec2(0.0, 3.0);
    content.add(
        egui::Label::new(strong(&source.name, 13.0, pal.text_bright))
            .truncate()
            .selectable(false),
    );
    content.add(
        egui::Label::new(mono_text(&source.detail, 10.5, pal.text_dim))
            .truncate()
            .selectable(false),
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
