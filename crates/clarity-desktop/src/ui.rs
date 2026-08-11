//! Reusable pieces of the Clarity visual language — avatars, pills, status
//! dots, section labels, and the button styles — so screens read as
//! composition rather than raw painting.

use egui::{
    Align, Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Pos2, Rect, Response,
    RichText, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2,
};

use crate::theme::{Palette, bold, medium, mono};

/// Proportional text (Space Grotesk regular) at a size and color.
pub fn text(s: impl Into<String>, size: f32, color: Color32) -> RichText {
    RichText::new(s)
        .font(FontId::new(size, FontFamily::Proportional))
        .color(color)
}

/// Medium-weight text — names, labels, emphasis (Space Grotesk 500).
pub fn strong(s: impl Into<String>, size: f32, color: Color32) -> RichText {
    RichText::new(s).font(FontId::new(size, medium())).color(color)
}

/// Bold text — headings (Space Grotesk 700).
pub fn heading(s: impl Into<String>, size: f32, color: Color32) -> RichText {
    RichText::new(s).font(FontId::new(size, bold())).color(color)
}

/// Monospace text — the design's IBM Plex Mono for codes, metrics, labels.
pub fn mono_text(s: impl Into<String>, size: f32, color: Color32) -> RichText {
    RichText::new(s).font(FontId::new(size, mono())).color(color)
}

/// An uppercase, letter-spaced mono section label like "LIVE NOW".
pub fn section_label(ui: &mut Ui, pal: &Palette, label: &str, trailing: Option<&str>) {
    ui.horizontal(|ui| {
        ui.label(mono_text(label.to_uppercase(), 9.5, pal.text_dim));
        if let Some(t) = trailing {
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(mono_text(t, 9.5, pal.text_dim));
            });
        }
    });
}

/// A rounded square avatar with initials.
pub fn avatar(ui: &mut Ui, size: f32, initials: &str, bg: Color32, fg: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same((size * 0.28) as u8), bg);
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initials,
        FontId::new(size * 0.42, FontFamily::Proportional),
        fg,
    );
    resp
}

/// A small filled circle — the design's presence/quality dots.
pub fn status_dot(ui: &mut Ui, diameter: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), diameter / 2.0, color);
}

/// A card container: rounded panel with a subtle border, the design's base
/// surface for grouped content.
pub fn card<R>(
    ui: &mut Ui,
    fill: Color32,
    border: Color32,
    inner: Margin,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    Frame::NONE
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, border))
        .corner_radius(CornerRadius::same(11))
        .inner_margin(inner)
        .show(ui, add)
        .inner
}

/// The primary violet action button.
pub fn accent_button(ui: &mut Ui, pal: &Palette, label: &str, min_height: f32) -> Response {
    filled_button(
        ui,
        label,
        pal.on_accent,
        pal.accent,
        pal.accent,
        min_height,
        13.0,
    )
}

/// A neutral raised button ("Join by link", "Join").
pub fn neutral_button(ui: &mut Ui, pal: &Palette, label: &str, min_height: f32) -> Response {
    filled_button(
        ui,
        label,
        pal.text,
        pal.raised,
        pal.border_strong,
        min_height,
        12.5,
    )
}

#[allow(dead_code)]
/// A transparent, bordered button ("Theatre", "Diagnostics").
pub fn outline_button(ui: &mut Ui, pal: &Palette, label: &str, min_height: f32) -> Response {
    filled_button(
        ui,
        label,
        pal.text_muted,
        Color32::TRANSPARENT,
        pal.border,
        min_height,
        12.0,
    )
}

fn filled_button(
    ui: &mut Ui,
    label: &str,
    fg: Color32,
    fill: Color32,
    border: Color32,
    min_height: f32,
    size: f32,
) -> Response {
    let padding = vec2(14.0, 0.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::new(size, medium()), fg);
    let desired = vec2(galley.size().x + padding.x * 2.0, min_height);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
    let hovered = resp.hovered();
    let bg = if hovered && fill != Color32::TRANSPARENT {
        lighten(fill, 0.06)
    } else if hovered {
        Color32::from_rgba_unmultiplied(255, 255, 255, 16)
    } else {
        fill
    };
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(8),
        bg,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    p.galley(rect.center() - galley.size() / 2.0, galley, fg);
    resp
}

pub(crate) fn lighten(c: Color32, amount: f32) -> Color32 {
    let f = |v: u8| {
        ((v as f32) + (255.0 - v as f32) * amount)
            .round()
            .min(255.0) as u8
    };
    Color32::from_rgba_unmultiplied(f(c.r()), f(c.g()), f(c.b()), c.a())
}

#[allow(dead_code)]
/// A full-width vertical separator line at the given color.
pub fn v_divider(ui: &mut Ui, height: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, height), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, color);
}

/// A form field's label — the medium-weight caption sitting above an input.
pub fn field_label(ui: &mut Ui, pal: &Palette, label: &str) {
    ui.label(strong(label, 11.5, pal.text_muted));
}

/// Up to two uppercase initials from a display name, for avatars. Falls back to
/// `?` for a nameless contact.
pub fn initials(name: &str) -> String {
    let letters: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if letters.is_empty() {
        "?".to_owned()
    } else {
        letters
    }
}

/// An editable single-line text field with the app's field styling (page-filled
/// rounded box, accent focus). Returns the `TextEdit` response so callers can
/// react to `changed()`/`lost_focus()`. `mono_face` selects IBM Plex Mono for
/// codes and URLs.
pub fn text_field(
    ui: &mut Ui,
    pal: &Palette,
    buffer: &mut String,
    hint: &str,
    height: f32,
    font_size: f32,
    mono_face: bool,
) -> Response {
    let family = if mono_face {
        mono()
    } else {
        FontFamily::Proportional
    };
    let mut response = None;
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        v.extreme_bg_color = pal.page;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, pal.border);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, pal.border_strong);
        v.widgets.active.bg_stroke = Stroke::new(1.0, pal.accent.gamma_multiply(0.7));
        v.widgets.inactive.corner_radius = CornerRadius::same(8);
        v.widgets.hovered.corner_radius = CornerRadius::same(8);
        v.widgets.active.corner_radius = CornerRadius::same(8);
        let vertical = ((height - font_size) / 2.0).round().clamp(4.0, 24.0) as i8;
        let edit = egui::TextEdit::singleline(buffer)
            .hint_text(hint)
            .desired_width(f32::INFINITY)
            .min_size(vec2(0.0, height))
            .vertical_align(Align::Center)
            .margin(Margin::symmetric(13, vertical))
            .font(FontId::new(font_size, family))
            .text_color(pal.text_bright)
            .background_color(pal.page);
        response = Some(ui.add(edit));
    });
    response.expect("text field always produces a response")
}

/// A checkbox with a title and a muted one-line explanation beside it. Returns
/// true on the frame it is toggled so callers can persist the change.
pub fn checkbox_row(
    ui: &mut Ui,
    pal: &Palette,
    checked: bool,
    title: &str,
    sub: &str,
) -> bool {
    ui.horizontal_top(|ui| {
        let (box_rect, resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
        let box_rect = box_rect.translate(vec2(0.0, 2.0));
        if checked {
            ui.painter()
                .rect_filled(box_rect, CornerRadius::same(4), pal.accent);
            // A check mark drawn as two strokes.
            let c = box_rect.center();
            ui.painter().add(Shape::line(
                vec![
                    c + vec2(-4.0, 0.0),
                    c + vec2(-1.0, 3.0),
                    c + vec2(4.0, -3.5),
                ],
                Stroke::new(1.8_f32, pal.on_accent),
            ));
        } else {
            ui.painter().rect(
                box_rect,
                CornerRadius::same(4),
                Color32::TRANSPARENT,
                Stroke::new(1.0_f32, pal.border_strong),
                egui::StrokeKind::Inside,
            );
        }
        ui.add_space(11.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 4.0);
            ui.label(text(title, 12.5, pal.text));
            ui.label(text(sub, 11.5, pal.text_dim));
        });
        resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
    })
    .inner
}

/// A small downward chevron, painted because the ▾ glyph is absent from the
/// bundled fonts. `center` is the triangle's centroid.
pub fn chevron_down(painter: &egui::Painter, center: Pos2, color: Color32) {
    let (w, h) = (9.0, 5.0);
    painter.add(Shape::convex_polygon(
        vec![
            center + vec2(-w / 2.0, -h / 2.0),
            center + vec2(w / 2.0, -h / 2.0),
            center + vec2(0.0, h / 2.0),
        ],
        color,
        Stroke::NONE,
    ));
}

/// Fills `outer` except for `hole`, which stays unpainted, using up to four
/// axis-aligned rectangles (above, below, left of, and right of the hole).
/// The native video path uses this to keep a region of the window transparent
/// so the subsurface below shows through.
pub fn fill_around(painter: &egui::Painter, outer: Rect, hole: Rect, color: Color32) {
    let hole = hole.intersect(outer);
    if !hole.is_positive() {
        painter.rect_filled(outer, CornerRadius::ZERO, color);
        return;
    }
    let band = |min: Pos2, max: Pos2| {
        let rect = Rect::from_min_max(min, max);
        if rect.is_positive() {
            painter.rect_filled(rect, CornerRadius::ZERO, color);
        }
    };
    band(outer.left_top(), pos2(outer.right(), hole.top()));
    band(pos2(outer.left(), hole.bottom()), outer.right_bottom());
    band(pos2(outer.left(), hole.top()), pos2(hole.left(), hole.bottom()));
    band(pos2(hole.right(), hole.top()), pos2(outer.right(), hole.bottom()));
}

/// Fills `rect` with the design's diagonal two-tone hatch, approximating its
/// `repeating-linear-gradient` screen-share placeholder. `band` is the stripe
/// width; the second tone `b` is painted over the base `a`.
pub fn paint_hatch(painter: &egui::Painter, rect: Rect, a: Color32, b: Color32, band: f32) {
    painter.rect_filled(rect, CornerRadius::ZERO, a);
    let mut clip = painter.clone();
    clip.set_clip_rect(rect);
    let step = band * 2.0;
    let slant = rect.height();
    let mut x = rect.left() - slant;
    while x < rect.right() + slant {
        clip.add(Shape::convex_polygon(
            vec![
                pos2(x, rect.bottom()),
                pos2(x + band, rect.bottom()),
                pos2(x + band + slant, rect.top()),
                pos2(x + slant, rect.top()),
            ],
            b,
            Stroke::NONE,
        ));
        x += step;
    }
}
