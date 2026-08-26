//! User-editable client settings. `#[serde(default)]` means a config file
//! written by an older build gains new fields at their defaults rather than
//! failing to load.

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureProfile {
    /// Sharp at 30 fps — the default, tuned for text and UI.
    #[default]
    Text,
    /// Smooth at 60 fps — for motion.
    Motion,
}

impl CaptureProfile {
    pub fn title(self) -> &'static str {
        match self {
            CaptureProfile::Text => "Text",
            CaptureProfile::Motion => "Motion",
        }
    }

    /// The one-line label used in the picker, e.g. "Text · 30 fps".
    pub fn label(self) -> &'static str {
        match self {
            CaptureProfile::Text => "Text · 30 fps",
            CaptureProfile::Motion => "Motion · 60 fps",
        }
    }

    pub fn fps(self) -> u32 {
        match self {
            CaptureProfile::Text => 30,
            CaptureProfile::Motion => 60,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub signaling_server: String,
    pub capture_profile: CaptureProfile,
    pub max_capture: String,
    pub include_system_audio: bool,
    pub always_relay: bool,
    /// Video codec preference for sharing, best first, as stable lowercase
    /// ids (`av1`, `h265`, `h264`, `vp9`, `vp8`). Every ranked codec the
    /// machine can encode is offered; the viewer's answer picks the first it
    /// can decode. An empty list means the engine's default order. Unknown
    /// ids are ignored, so a newer build's ranking loads harmlessly here.
    pub codec_ranking: Vec<String>,
    /// Friend codes whose incoming requests were dismissed, so the server
    /// re-reporting them (it pushes the full pending set on every connect)
    /// does not re-nag. Dismissal is local only; the requester is not told.
    pub dismissed_requests: Vec<String>,
}

impl Settings {
    /// The "Max capture" choice as pixel dimensions (width, height). An
    /// unrecognized stored value falls back to the 2560 × 1440 default.
    #[must_use]
    pub fn max_capture_dimensions(&self) -> (u32, u32) {
        if self.max_capture.starts_with("1920") {
            (1920, 1080)
        } else {
            (2560, 1440)
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // The Clarity signaling server this device talks to. A neutral local
            // default; set to your own deployment on the first-run screen or in
            // Settings. A room is created against it.
            signaling_server: "http://127.0.0.1:3000".to_owned(),
            capture_profile: CaptureProfile::Text,
            max_capture: "2560 × 1440".to_owned(),
            include_system_audio: true,
            always_relay: false,
            codec_ranking: Vec::new(),
            dismissed_requests: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tolerates_missing_fields() {
        // A file from an older build with only one known field still loads.
        let settings: Settings = serde_json::from_str(r#"{"always_relay":true}"#).expect("parse");
        assert!(settings.always_relay);
        assert_eq!(
            settings.signaling_server,
            Settings::default().signaling_server
        );
        assert_eq!(settings.capture_profile, CaptureProfile::Text);
    }

    #[test]
    fn codec_ranking_defaults_to_empty() {
        let settings: Settings = serde_json::from_str("{}").expect("parse");
        assert!(settings.codec_ranking.is_empty());
    }

    #[test]
    fn max_capture_parses_to_dimensions() {
        let mut settings = Settings::default();
        assert_eq!(settings.max_capture_dimensions(), (2560, 1440));
        settings.max_capture = "1920 × 1080".to_owned();
        assert_eq!(settings.max_capture_dimensions(), (1920, 1080));
        settings.max_capture = "garbage".to_owned();
        assert_eq!(settings.max_capture_dimensions(), (2560, 1440));
    }
}
