//! Sender-side adaptive rate control.
//!
//! Two paths share this module. A viewer driven by the vendored
//! `claritygccbwe` element trusts the estimate as-is ([`trust_estimate`]):
//! that element detects the application-limited region itself, holds its
//! target through idle periods, and re-measures the link on exit, so a
//! wrapper second-guessing it would only fight it — most visibly by reading
//! the legitimate settle-down decrease after an idle period as congestion
//! and backing off on top of it. Everything below `trust_estimate` is the
//! fallback for the stock `rtpgccbwe`: an application-limited-region (ALR)
//! aware wrapper over the estimator, reproducing the core of how a browser
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
//! reproduces the probe half with real content rather than synthetic padding:
//! the encoder runs in VBR with its peak held above the target, so a static
//! screen costs almost nothing and a busy screen bursts past the current
//! belief — and that real burst is what re-measures the link. (webrtcbin
//! exposes no pacer to inject probe packets, and NVENC's VBR will not pad a
//! static frame, so padding-based probing is not available here.)
//!
//! The stock estimator's absolute value goes stale while the sender is
//! application-limited: its state cannot be corrected from outside
//! (`estimated-bitrate` is only writable before the stock element starts;
//! the vendored one accepts live writes but does not need them), so after
//! an idle period it reports a collapsed number and climbs back at only ~8%/s.
//! Adopting that number on the first busy frame would crush the encoder
//! exactly when the user starts doing something. The controller therefore
//! leaves ALR into a *validating* state: capacity is held while the estimate
//! climbs, a sustained estimator decrease is treated as fresh congestion
//! evidence and answered with a multiplicative backoff from the measured send
//! rate (the estimator's own response, re-anchored to a number that is not
//! stale), and absolute tracking resumes only once the estimate has caught
//! back up to the held capacity.

use std::time::{Duration, Instant};

/// Below this fraction of the current target, the encoder is judged
/// application-limited (idle / static content) rather than network-limited.
const ALR_FRACTION: f32 = 0.75;

/// `kbps * num / den`, widened so a large configured ceiling cannot overflow.
fn frac(kbps: u32, num: u64, den: u64) -> u32 {
    u32::try_from(u64::from(kbps) * num / den).unwrap_or(u32::MAX)
}

/// The encoder rate to apply after folding in one estimator update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RateCommand {
    /// The encoder's average bitrate target, in kbps.
    pub target_kbps: u32,
    /// The encoder's peak/max bitrate (the VBR cap), in kbps.
    pub max_kbps: u32,
}

/// The encoder rate for a viewer whose estimator manages the
/// application-limited region itself (the vendored `claritygccbwe`): the
/// estimate is current by construction, so it is applied directly, with the
/// same VBR peak headroom the controller keeps so that real content can
/// burst above the target and re-measure the link.
pub(crate) fn trust_estimate(
    estimate_kbps: u32,
    floor_kbps: u32,
    ceiling_kbps: u32,
) -> RateCommand {
    let ceiling_kbps = floor_kbps.max(ceiling_kbps);
    let target_kbps = estimate_kbps.clamp(floor_kbps, ceiling_kbps);
    RateCommand {
        target_kbps,
        max_kbps: frac(target_kbps, 3, 2).min(ceiling_kbps),
    }
}

/// How much the estimator's absolute value is currently trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Belief {
    /// The estimate has been at capacity since the last application-limited
    /// period: it is live, and ordinary congestion control applies.
    Tracking,
    /// The estimate collapsed during an application-limited period and has not
    /// caught back up: its absolute value is stale. `peak_kbps` is the highest
    /// estimate seen since it started recovering; a drop well below that peak
    /// is fresh overuse evidence even though the absolute value is not.
    Validating { peak_kbps: u32 },
}

/// Tracks a belief about the link's video capacity, updated from the congestion
/// estimator but held steady while the encoder is application-limited.
pub(crate) struct AdaptiveController {
    floor_kbps: u32,
    ceiling_kbps: u32,
    capacity_kbps: u32,
    belief: Belief,
}

impl AdaptiveController {
    pub fn new(floor_kbps: u32, ceiling_kbps: u32, start_kbps: u32) -> Self {
        let ceiling_kbps = floor_kbps.max(ceiling_kbps);
        Self {
            floor_kbps,
            ceiling_kbps,
            capacity_kbps: start_kbps.clamp(floor_kbps, ceiling_kbps),
            // The estimator is seeded with the starting rate, so it opens in
            // sync with the belief.
            belief: Belief::Tracking,
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
            // estimator's low reading reflects a lack of content, not
            // congestion. Hold capacity; accept only an increase. A collapsed
            // reading also marks the estimator stale, so the next busy period
            // starts by validating it rather than trusting it.
            if estimate_kbps > self.capacity_kbps {
                self.capacity_kbps = estimate_kbps;
                self.belief = Belief::Tracking;
            } else if estimate_kbps < frac(self.capacity_kbps, 9, 10) {
                self.belief = Belief::Validating {
                    peak_kbps: estimate_kbps,
                };
            }
        } else {
            match self.belief {
                // Real content is saturating the encoder and the estimate is
                // live, so it reflects the true link: track it up when there
                // is headroom and back off at once when it drops. This is
                // ordinary congestion control.
                Belief::Tracking => self.capacity_kbps = estimate_kbps,
                Belief::Validating { peak_kbps } => {
                    if estimate_kbps >= frac(self.capacity_kbps, 9, 10) {
                        // The burst validated the held capacity: the estimate
                        // caught up, so its absolute value is live again.
                        self.capacity_kbps = self.capacity_kbps.max(estimate_kbps);
                        self.belief = Belief::Tracking;
                    } else if estimate_kbps < frac(peak_kbps, 23, 25) {
                        // The estimator pushed back down against a real burst:
                        // fresh overuse. Its absolute value is still stale
                        // (one decrease is a 5% step off a collapsed number),
                        // so apply its multiplicative backoff to the measured
                        // send rate instead — the same response, re-anchored.
                        // The 0.92 trigger needs two consecutive decreases, so
                        // a single spurious detection cannot fire it.
                        self.capacity_kbps = self.capacity_kbps.min(frac(actual_send_kbps, 17, 20));
                        self.belief = Belief::Validating {
                            peak_kbps: estimate_kbps,
                        };
                    } else {
                        // Still climbing back toward the held capacity: hold,
                        // and remember the high-water mark.
                        self.belief = Belief::Validating {
                            peak_kbps: peak_kbps.max(estimate_kbps),
                        };
                    }
                }
            }
        }
        self.capacity_kbps = self.capacity_kbps.clamp(self.floor_kbps, self.ceiling_kbps);

        RateCommand {
            target_kbps: self.capacity_kbps,
            // Peak headroom above the target lets a busy screen burst past the
            // current belief; the estimator can only raise what it can measure.
            max_kbps: frac(self.capacity_kbps, 3, 2).min(self.ceiling_kbps),
        }
    }

    #[cfg(test)]
    pub fn capacity_kbps(&self) -> u32 {
        self.capacity_kbps
    }
}

/// Measures the video send rate from a monotonically growing byte counter,
/// sampled on the estimator's update cadence but averaged over a fixed window.
/// This replaces the 2-second stats poll as the controller's send-rate input:
/// ALR classification needs a reading fresh enough to catch a busy/static
/// transition before the estimator reacts to it.
pub(crate) struct SendRateSampler {
    window_start: Option<(u64, Instant)>,
    kbps: u32,
}

/// Wide enough to smooth frame-size jitter, narrow enough that a busy/static
/// transition is seen before the estimator's 1-second receive window turns
/// over and anchors a decrease to the collapsed content rate.
const SAMPLE_WINDOW: Duration = Duration::from_millis(500);

impl SendRateSampler {
    pub fn new() -> Self {
        Self {
            window_start: None,
            kbps: 0,
        }
    }

    /// Folds in the byte counter's current reading and returns the rate over
    /// the last completed window; zero until the first window completes.
    pub fn sample(&mut self, bytes_sent: u64, now: Instant) -> u32 {
        let Some((window_bytes, window_at)) = self.window_start else {
            self.window_start = Some((bytes_sent, now));
            return 0;
        };
        let elapsed = now.saturating_duration_since(window_at);
        if elapsed >= SAMPLE_WINDOW {
            let bits = bytes_sent.saturating_sub(window_bytes).saturating_mul(8);
            // Bits per millisecond is kbit per second.
            let millis = u64::try_from(elapsed.as_millis())
                .unwrap_or(u64::MAX)
                .max(1);
            self.kbps = u32::try_from(bits / millis).unwrap_or(u32::MAX);
            self.window_start = Some((bytes_sent, now));
        }
        self.kbps
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
        assert_eq!(
            ctl.capacity_kbps(),
            4_500,
            "idle estimate must not lower capacity"
        );
    }

    // With real content saturating the encoder and a live estimate, a genuine
    // congestion drop is honored immediately.
    #[test]
    fn backs_off_on_real_congestion() {
        let mut ctl = AdaptiveController::new(600, 6_000, 4_500);
        // Sending near capacity (not application-limited); estimate drops.
        let cmd = ctl.on_estimate(2_000, 4_400);
        assert_eq!(cmd.target_kbps, 2_000);
        assert_eq!(
            cmd.max_kbps, 3_000,
            "the VBR peak keeps burst headroom above the target"
        );
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
        assert_eq!(
            ctl.capacity_kbps(),
            4_500,
            "the idle source must not fight the controller"
        );
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

    // The core of the stuck-low symptom: after an idle period the estimate is
    // collapsed and only climbs ~8%/s, so the first busy frames must not have
    // capacity snapped down to it. Hold until the estimate catches up, then
    // resume ordinary tracking.
    #[test]
    fn alr_exit_holds_until_the_estimate_catches_up() {
        let mut ctl = AdaptiveController::new(600, 6_000, 4_500);
        for _ in 0..10 {
            ctl.on_estimate(700, 300); // static screen; estimate collapsed
        }
        // Content turns busy while the estimate is still stale.
        let cmd = ctl.on_estimate(750, 4_400);
        assert_eq!(
            cmd.target_kbps, 4_500,
            "the collapsed estimate must not crush the burst"
        );
        // The estimate climbs back; capacity holds the whole way up.
        for estimate in [900, 1_500, 2_500, 3_900] {
            assert_eq!(ctl.on_estimate(estimate, 4_400).target_kbps, 4_500);
        }
        // Caught up (>= 90% of capacity): validated.
        assert_eq!(ctl.on_estimate(4_100, 4_400).target_kbps, 4_500);
        // Ordinary tracking resumes, decreases included.
        assert_eq!(ctl.on_estimate(4_000, 4_300).target_kbps, 4_000);
    }

    // While validating, a sustained estimator decrease is real overuse
    // evidence even though the absolute estimate is stale: back off from the
    // measured send rate, the way the estimator itself would.
    #[test]
    fn congestion_while_validating_backs_off_from_the_send_rate() {
        let mut ctl = AdaptiveController::new(600, 6_000, 4_500);
        for _ in 0..5 {
            ctl.on_estimate(700, 300);
        }
        ctl.on_estimate(750, 4_400); // busy onset; hold at 4_500, peak 750
        // Two estimator decreases (below 92% of the peak) against the burst.
        let cmd = ctl.on_estimate(680, 4_400);
        assert_eq!(cmd.target_kbps, 3_740, "0.85 of the measured send rate");
    }

    // A single 5% estimator decrease can be spurious (idle jitter); it must
    // not trigger the backoff on its own.
    #[test]
    fn spurious_single_decrease_while_validating_is_ignored() {
        let mut ctl = AdaptiveController::new(600, 6_000, 4_500);
        for _ in 0..5 {
            ctl.on_estimate(700, 300);
        }
        ctl.on_estimate(750, 4_400); // busy onset; peak 750
        let cmd = ctl.on_estimate(715, 4_400); // one 0.95x step: within the band
        assert_eq!(cmd.target_kbps, 4_500);
    }

    // The VBR peak carries 1.5x headroom over the target, capped at the
    // ceiling, so a burst can overshoot the current belief and be measured.
    #[test]
    fn peak_headroom_is_capped_at_the_ceiling() {
        let mut ctl = AdaptiveController::new(600, 6_000, 4_500);
        let cmd = ctl.on_estimate(4_500, 4_400);
        assert_eq!(cmd.max_kbps, 6_000, "1.5x of 4_500 exceeds the ceiling");
    }

    // The vendored element's estimate is applied directly, with the same
    // peak headroom the controller keeps.
    #[test]
    fn trusted_estimate_is_applied_with_peak_headroom() {
        let cmd = trust_estimate(2_000, 600, 6_000);
        assert_eq!(cmd.target_kbps, 2_000);
        assert_eq!(cmd.max_kbps, 3_000);
    }

    #[test]
    fn trusted_estimate_clamps_to_floor_and_ceiling() {
        assert_eq!(trust_estimate(50, 600, 6_000).target_kbps, 600);
        let cmd = trust_estimate(9_000, 600, 6_000);
        assert_eq!(cmd.target_kbps, 6_000);
        assert_eq!(cmd.max_kbps, 6_000, "headroom never exceeds the ceiling");
    }

    #[test]
    fn samples_rate_over_the_window() {
        let mut sampler = SendRateSampler::new();
        let t0 = Instant::now();
        assert_eq!(sampler.sample(0, t0), 0);
        assert_eq!(
            sampler.sample(10_000, t0 + Duration::from_millis(100)),
            0,
            "no rate until the first window completes"
        );
        // 250 kB over 500 ms is 4_000 kbit/s.
        assert_eq!(
            sampler.sample(250_000, t0 + Duration::from_millis(500)),
            4_000
        );
        // Mid-window reads return the last completed window's rate.
        assert_eq!(
            sampler.sample(260_000, t0 + Duration::from_millis(600)),
            4_000
        );
        // A near-idle window drops the rate promptly.
        assert_eq!(
            sampler.sample(260_000, t0 + Duration::from_millis(1_000)),
            160
        );
    }
}
