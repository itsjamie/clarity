//! Sender-side adaptive rate control: an application-limited-region (ALR) aware
//! wrapper over the congestion estimator, reproducing the core of how a browser
//! sender keeps a screen share at a good bitrate.
//!
//! The Google congestion estimator ([`rtpgccbwe`]) cannot measure capacity above
//! the rate actually being sent, so on low-complexity (mostly static) screen
//! content it reads the small encoder output as the link ceiling and collapses
//! the estimate — the ~800 kbps trap. A browser avoids this two ways: it detects
//! the *application-limited region* — the encoder is sending little because there
//! is nothing to encode, not because the network is congested — and it probes
//! for headroom with padding packets.
//!
//! This reproduces the ALR half faithfully: while application-limited it HOLDS
//! the measured capacity instead of letting an idle estimate drop it. It
//! reproduces the probe half with real content rather than synthetic padding: the
//! encoder runs in VBR, so a static screen costs almost nothing and a busy screen
//! bursts up to the held capacity — and that real burst is what re-measures the
//! link. (webrtcbin exposes no pacer to inject probe packets, and NVENC's VBR
//! will not pad a static frame, so padding-based probing is not available here.)

/// Below this fraction of the current target, the encoder is judged
/// application-limited (idle / static content) rather than network-limited.
const ALR_FRACTION: f32 = 0.75;

/// The encoder rate to apply after folding in one estimator update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RateCommand {
    /// The encoder's average bitrate target, in kbps.
    pub target_kbps: u32,
    /// The encoder's peak/max bitrate (the VBR cap), in kbps.
    pub max_kbps: u32,
}

/// Tracks a belief about the link's video capacity, updated from the congestion
/// estimator but held steady while the encoder is application-limited.
pub(crate) struct AdaptiveController {
    floor_kbps: u32,
    ceiling_kbps: u32,
    capacity_kbps: u32,
}

impl AdaptiveController {
    pub fn new(floor_kbps: u32, ceiling_kbps: u32, start_kbps: u32) -> Self {
        let ceiling_kbps = floor_kbps.max(ceiling_kbps);
        Self {
            floor_kbps,
            ceiling_kbps,
            capacity_kbps: start_kbps.clamp(floor_kbps, ceiling_kbps),
        }
    }

    /// Folds one estimator update into the capacity belief and returns the
    /// encoder rate to apply. `estimate_kbps` is the estimator's current video
    /// bandwidth estimate; `actual_send_kbps` is the encoder's recent measured
    /// video output.
    pub fn on_estimate(&mut self, estimate_kbps: u32, actual_send_kbps: u32) -> RateCommand {
        let application_limited =
            (actual_send_kbps as f32) < ALR_FRACTION * self.capacity_kbps as f32;

        if application_limited {
            // Idle or static: the encoder is sending little by choice, so the
            // estimator's low reading reflects a lack of content, not congestion.
            // Hold capacity; accept only an increase (the estimator rarely raises
            // while there is nothing to measure).
            if estimate_kbps > self.capacity_kbps {
                self.capacity_kbps = estimate_kbps;
            }
        } else {
            // Real content is saturating the encoder, so the estimate reflects
            // the true link: track it up when there is headroom and back off at
            // once when it drops. This is ordinary congestion control.
            self.capacity_kbps = estimate_kbps;
        }
        self.capacity_kbps = self.capacity_kbps.clamp(self.floor_kbps, self.ceiling_kbps);

        RateCommand {
            target_kbps: self.capacity_kbps,
            max_kbps: self.capacity_kbps,
        }
    }

    #[cfg(test)]
    pub fn capacity_kbps(&self) -> u32 {
        self.capacity_kbps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A static screen sends far below the target; the idle estimate must not
    // drag capacity down (the ~800 kbps trap).
    #[test]
    fn holds_capacity_while_application_limited() {
        let mut ctl = AdaptiveController::new(600, 6_000, 4_500);
        // Encoder sends only 300 kbps of a static screen; estimate collapses.
        for _ in 0..10 {
            ctl.on_estimate(800, 300);
        }
        assert_eq!(ctl.capacity_kbps(), 4_500, "idle estimate must not lower capacity");
    }

    // With real content saturating the encoder, a genuine congestion drop is
    // honored immediately.
    #[test]
    fn backs_off_on_real_congestion() {
        let mut ctl = AdaptiveController::new(600, 6_000, 4_500);
        // Sending near capacity (not application-limited); estimate drops.
        let cmd = ctl.on_estimate(2_000, 4_400);
        assert_eq!(cmd.target_kbps, 2_000);
        assert_eq!(cmd.max_kbps, 2_000);
    }

    // A busy screen re-measures the link: capacity climbs to the estimate.
    #[test]
    fn tracks_up_when_content_fills_the_pipe() {
        let mut ctl = AdaptiveController::new(600, 6_000, 1_000);
        // Content bursts to the held capacity, estimator now measures headroom.
        ctl.on_estimate(3_800, 950);
        assert_eq!(ctl.capacity_kbps(), 3_800);
    }

    // The idle placeholder source sends almost nothing; its collapsed
    // estimate must not drag the held capacity down while the room sits idle.
    #[test]
    fn holds_capacity_on_the_idle_placeholder() {
        let mut ctl = AdaptiveController::new(600, 6_000, 4_500);
        for _ in 0..20 {
            ctl.on_estimate(600, 0);
        }
        assert_eq!(ctl.capacity_kbps(), 4_500, "the idle source must not fight the controller");
    }

    // The idle case still accepts an increase if the estimator raises.
    #[test]
    fn accepts_increase_while_idle() {
        let mut ctl = AdaptiveController::new(600, 6_000, 2_000);
        ctl.on_estimate(2_600, 100);
        assert_eq!(ctl.capacity_kbps(), 2_600);
    }

    #[test]
    fn clamps_to_floor_and_ceiling() {
        let mut ctl = AdaptiveController::new(600, 6_000, 4_000);
        ctl.on_estimate(50, 4_000); // below floor, not app-limited
        assert_eq!(ctl.capacity_kbps(), 600);
        ctl.on_estimate(99_000, 4_000); // above ceiling
        assert_eq!(ctl.capacity_kbps(), 6_000);
    }
}
