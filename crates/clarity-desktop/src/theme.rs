//! The Clarity design palette and typography.
//!
//! The design specifies colors in oklch; they are converted to sRGB once at
//! startup so the rest of the UI works in `egui::Color32`. Fonts are the same
//! two families the design uses: Space Grotesk for text, IBM Plex Mono for
//! codes, metrics, and labels.

use egui::{Color32, FontData, FontDefinitions, FontFamily};

/// Named family for IBM Plex Mono, registered alongside the proportional font.
pub fn mono() -> FontFamily {
    FontFamily::Name("mono".into())
}

/// Space Grotesk Medium (500) — names, labels, button text.
pub fn medium() -> FontFamily {
    FontFamily::Name("medium".into())
}

/// Space Grotesk Bold (700) — headings.
pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

/// The platform's command-modifier label. Space Grotesk has no ⌘ glyph, and on
/// Linux/Windows Ctrl is the honest key anyway.
pub fn mod_key() -> &'static str {
    if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" }
}

/// The platform's fullscreen shortcut, as shown in menus.
pub fn fullscreen_key() -> &'static str {
    if cfg!(target_os = "macos") { "⌃⌘F" } else { "F11" }
}

/// Every color the design uses, resolved from oklch to sRGB.
#[derive(Clone, Copy)]
#[allow(dead_code)] // some fields are used by screens landing in the next pass
pub struct Palette {
    pub page: Color32,
    pub window: Color32,
    pub panel: Color32,
    pub card: Color32,
    pub card_alt: Color32,
    pub input: Color32,
    pub stage: Color32,
    pub raised: Color32,

    pub accent: Color32,
    pub accent_dim: Color32,
    pub accent_wash: Color32,
    pub accent_text: Color32,
    pub on_accent: Color32,

    pub green: Color32,
    pub amber: Color32,
    pub red: Color32,

    pub text_bright: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,

    pub border: Color32,
    pub border_soft: Color32,
    pub border_strong: Color32,
}

impl Palette {
    pub fn clarity() -> Self {
        Self {
            page: oklch(0.10, 0.004, 260.0),
            window: oklch(0.14, 0.005, 260.0),
            panel: oklch(0.125, 0.005, 260.0),
            card: oklch(0.17, 0.008, 260.0),
            card_alt: oklch(0.16, 0.006, 260.0),
            input: oklch(0.155, 0.006, 260.0),
            stage: oklch(0.085, 0.004, 260.0),
            raised: oklch(0.20, 0.01, 260.0),

            accent: oklch(0.64, 0.19, 288.0),
            accent_dim: oklch_a(0.64, 0.19, 288.0, 0.16),
            accent_wash: oklch_a(0.64, 0.19, 288.0, 0.10),
            accent_text: oklch(0.90, 0.05, 288.0),
            on_accent: oklch(0.17, 0.03, 288.0),

            green: oklch(0.72, 0.14, 150.0),
            amber: oklch(0.79, 0.13, 80.0),
            red: oklch(0.68, 0.15, 25.0),

            text_bright: oklch(0.96, 0.004, 260.0),
            text: oklch(0.85, 0.008, 260.0),
            text_muted: oklch(0.66, 0.012, 260.0),
            text_dim: oklch(0.52, 0.012, 260.0),
            text_faint: oklch(0.45, 0.012, 260.0),

            border: white_alpha(0.09),
            border_soft: white_alpha(0.07),
            border_strong: white_alpha(0.16),
        }
    }
}

/// Installs Space Grotesk (proportional default) and IBM Plex Mono (both the
/// `Monospace` family and a named `mono` family) into egui's font set.
pub fn install_fonts(ctx: &egui::Context) {
    use std::sync::Arc;
    let mut fonts = FontDefinitions::default();
    let mut load = |name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    // Three static Space Grotesk weights (ab_glyph renders a variable font's
    // base master only, so weight comes from separate files) plus Plex Mono.
    load("sg", include_bytes!("../assets/fonts/SpaceGrotesk-400.ttf"));
    load("sg-medium", include_bytes!("../assets/fonts/SpaceGrotesk-500.ttf"));
    load("sg-bold", include_bytes!("../assets/fonts/SpaceGrotesk-700.ttf"));
    load("plex-mono", include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"));

    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "sg".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "plex-mono".to_owned());
    // Heavier weights fall back to regular for any missing glyph.
    for (family, primary) in [("mono", "plex-mono"), ("medium", "sg-medium"), ("bold", "sg-bold")] {
        fonts.families.insert(
            FontFamily::Name(family.into()),
            vec![primary.to_owned(), "sg".to_owned()],
        );
    }

    ctx.set_fonts(fonts);
}

/// Converts an opaque oklch color to sRGB.
fn oklch(l: f32, c: f32, h_deg: f32) -> Color32 {
    oklch_a(l, c, h_deg, 1.0)
}

/// Converts an oklch color with alpha to a premultiplied-safe sRGB color.
fn oklch_a(l: f32, c: f32, h_deg: f32, alpha: f32) -> Color32 {
    let h = h_deg.to_radians();
    let (a, b) = (c * h.cos(), c * h.sin());

    // oklab -> linear sRGB (Björn Ottosson).
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3;
    let g = -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3;
    let bl = -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3;

    let a8 = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(bl), a8)
}

fn linear_to_srgb(x: f32) -> u8 {
    let x = x.clamp(0.0, 1.0);
    let s = if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    };
    (s.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// White at the given alpha — the design draws borders as `oklch(1 0 0 / a)`.
fn white_alpha(alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, (alpha * 255.0).round() as u8)
}
