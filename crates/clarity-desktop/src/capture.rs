//! Offscreen self-capture, used to verify the rendered UI without depending on
//! the compositor's window focus.
//!
//! When `CLARITY_SHOT=<path>` is set, the app renders a few frames to let fonts
//! and layout settle, asks egui for its own framebuffer, writes it to a PNG,
//! and closes. This is the honest picture of what the app actually paints —
//! the same pixels the GPU shows — so it sidesteps focus-stealing prevention
//! and the "which window is active" guesswork of external screenshot tools.

use std::path::PathBuf;

use eframe::egui;

/// Drives the capture lifecycle if requested; otherwise does nothing.
pub struct Capture {
    path: Option<PathBuf>,
    frame: u32,
    at_frame: u32,
}

impl Capture {
    /// Reads `CLARITY_SHOT`. Absent env var → a no-op capture. `CLARITY_SHOT_FRAME`
    /// delays the shot to let animated content (like live video) settle.
    pub fn from_env() -> Self {
        let at_frame = std::env::var("CLARITY_SHOT_FRAME")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);
        Self {
            path: std::env::var_os("CLARITY_SHOT").map(PathBuf::from),
            frame: 0,
            at_frame,
        }
    }

    /// Call once per frame. Advances the capture state machine: settle, request,
    /// then save-and-close when the screenshot event arrives.
    pub fn tick(&mut self, ctx: &egui::Context) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.frame += 1;
        // Keep the UI animating so frames advance without user input.
        ctx.request_repaint();

        // Give layout, fonts, and the first paint a few frames to settle.
        if self.frame == self.at_frame {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }

        // The requested image comes back as an input event a frame or two later.
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            if let Err(err) = write_png(&path, &image) {
                eprintln!("clarity: screenshot failed: {err}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// Encodes an egui `ColorImage` (RGBA, sRGB) to a PNG file.
fn write_png(path: &std::path::Path, image: &egui::ColorImage) -> std::io::Result<()> {
    let [w, h] = [image.width() as u32, image.height() as u32];
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    // `as_raw` is the pixels' RGBA bytes in row-major order — exactly PNG's layout.
    writer
        .write_image_data(image.as_raw())
        .map_err(|e| std::io::Error::other(e.to_string()))
}
