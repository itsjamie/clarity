//! This algorithm is not described in
//! https://datatracker.ietf.org/doc/html/draft-ietf-rmcat-gcc-02. Instead, this
//! algorithm can be found in the GCC implementation in libwrtc, which is used
//! by e.g. Chromium.
//!
//! To explore the algorithm in libwebrtc, use the
//! `delay_detector_for_packet->Update(...)` call in
//! <https://webrtc.googlesource.com/src/+/refs/heads/main/modules/congestion_controller/goog_cc/delay_based_bwe.cc>
//! as a starting point. That call invokes `TrendlineEstimator::Update()` in
//! <https://webrtc.googlesource.com/src/+/refs/heads/main/modules/congestion_controller/goog_cc/trendline_estimator.cc>
//! which uses the same core algorithm as we do here.

use gstreamer as gst;

use super::Duration;
use super::EstimatorImpl;
use super::PacketGroup;

mod linear_regression;
use linear_regression::Samples;

const DEFAULT_SAMPLES_MAX_LEN: usize = 20;

// Must be between 0.0 and 1.0.
const SMOOTHING_FACTOR: f64 = 0.9;

// Since our estimate is a slope we need to amplify our estimate somewhat to
// match the dynamic threshold our estimate is compared against.
const GAIN: f64 = 4.;

#[derive(Debug, PartialEq, Clone)]
pub struct LinearRegressionEstimator {
    /// Our (pre-GAIN'ed) output value..
    estimate: Duration,

    /// The last inter-group delay variation measurement.
    measure: Duration,

    /// The samples used for calculating the slope of `accumulated_delay` with
    /// simple linear regression.
    samples: linear_regression::Samples,

    /// The sum over time of inter-group delay variation measurements. The
    /// measurements will jump up a down a bit, but on average they will sum to
    /// 0, since otherwise the receiver and sender will drift away from each
    /// other. After network problems, this can drift away from 0. But once
    /// things stabilize, it is expected to maintain a constant value again over
    /// time. If it remains constant, the network conditions are good. If it
    /// begins to increase, it indicates over-use of the network. If it begins
    /// to decrease, it indicates that network problems are clearing up. Since
    /// we are interested in changes to this value over time, we are interested
    /// in the slope of this value over time. Its absolute value (height over
    /// the y axis) over time does not matter to us.
    accumulated_delay: Duration,

    /// To make `accumulated_delay` less sensitive to measurement noise and
    /// natural network variation, we apply a low-pass filter to
    /// `accumulated_delay`, and this is the filtered value. It is actually the
    /// slope of this field that we use, and not the slope of
    /// `accumulated_delay`.
    smoothed_delay: Duration,
}

impl Default for LinearRegressionEstimator {
    fn default() -> Self {
        Self::with_samples_max_len(DEFAULT_SAMPLES_MAX_LEN)
    }
}

impl LinearRegressionEstimator {
    fn with_samples_max_len(samples_max_len: usize) -> Self {
        Self {
            accumulated_delay: Duration::ZERO,
            smoothed_delay: Duration::ZERO,
            samples: Samples::with_max_len(samples_max_len),
            measure: Duration::ZERO,
            estimate: Duration::ZERO,
        }
    }
}

impl EstimatorImpl for LinearRegressionEstimator {
    fn update(&mut self, prev_group: &PacketGroup, group: &PacketGroup) {
        self.measure = group.inter_delay_variation(prev_group);
        self.accumulated_delay += self.measure;
        self.smoothed_delay = SMOOTHING_FACTOR * self.smoothed_delay
            + (1f64 - SMOOTHING_FACTOR) * self.accumulated_delay;

        gst::log!(
            super::CAT,
            "accumulated_delay: {} - smoothed_delay: {} - samples len: {}",
            self.accumulated_delay,
            self.smoothed_delay,
            self.samples.len(),
        );

        // This never panics because the `inter_delay_variation()` call above
        // already did this.
        let arrival_time = group.arrival.unwrap();

        // As long as both x and y use the same unit, the exact unit does not
        // matter. Go with seconds since f64 versions of seconds exists.
        self.samples.push(linear_regression::Sample {
            x: arrival_time.as_seconds_f64(),
            y: self.smoothed_delay.as_seconds_f64(),
        });

        // To avoid big movements in slope in the beginning, wait until we have
        // enough samples. It won't take long.
        if self.samples.full()
            && let Some(slope) = self.samples.slope()
        {
            // The slope is dimensionless, but pretend it is milliseconds. That
            // makes the algorithm work.
            self.estimate = Duration::nanoseconds((slope * 1_000_000f64) as i64) * GAIN;
        }
    }

    fn estimate(&self) -> Duration {
        self.estimate
    }

    fn measure(&self) -> Duration {
        self.measure
    }

    fn expect_fast_rate_change(&mut self) {
        // The slope is only meaningful over samples taken under the same
        // conditions, so start the window over rather than let the old ones
        // hold the slope down, the way libwebrtc replaces its
        // `TrendlineEstimator` outright. The estimate goes with them: until
        // the window is full again there is nothing to base it on, and a
        // leftover value would keep signalling the conditions that are gone.
        self.samples.clear();
        self.accumulated_delay = Duration::ZERO;
        self.smoothed_delay = Duration::ZERO;
        self.measure = Duration::ZERO;
        self.estimate = Duration::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_accumulation() {
        let max_len = DEFAULT_SAMPLES_MAX_LEN;
        let mut estimator = LinearRegressionEstimator::with_samples_max_len(max_len);

        let (prev_group, group) = with_inter_group_delay(Duration::ZERO);

        for times_updated in 1..max_len * 5 {
            estimator.update(&prev_group, &group);

            // Since inter_group_delay is 0 we expect the accumulated and
            // smoothed diff to remain zero
            assert_eq!(estimator.accumulated_delay, Duration::ZERO);
            assert_eq!(estimator.smoothed_delay, Duration::ZERO);

            // The linear regression sample size shall increase until we reach
            // the max len.
            assert_eq!(estimator.samples.len(), times_updated.min(max_len))
        }
    }

    #[test]
    fn test_small_accumulation() {
        let mut estimator = LinearRegressionEstimator::default();

        let inter_group_delay = Duration::milliseconds(10);
        let (prev_group, group) = with_inter_group_delay(inter_group_delay);

        for times_updated in 1..10i32 {
            estimator.update(&prev_group, &group);

            // We expect the accumulated delay to increase proportionally with
            // the number of times we called update().
            assert_eq!(
                estimator.accumulated_delay,
                inter_group_delay * times_updated,
            );
            // Don't bother checking smoothed_delay. It's a bit awkward to
            // predict it.
        }
    }

    /// The slope is only meaningful over samples taken under the same
    /// conditions, so widening starts the window over: nothing is estimated
    /// again until it has refilled with measurements of the new conditions.
    #[test]
    fn expect_fast_rate_change_starts_the_window_over() {
        let mut estimator = LinearRegressionEstimator::default();

        let mut prev_group = PacketGroup {
            packets: vec![],
            departure: Duration::milliseconds(1000),
            arrival: Some(Duration::milliseconds(1050)),
        };
        // Groups arriving further and further behind: a rising slope.
        let mut rising = |estimator: &mut LinearRegressionEstimator, groups: usize| {
            for _ in 0..groups {
                let group = PacketGroup {
                    packets: vec![],
                    departure: prev_group.departure + Duration::milliseconds(100),
                    arrival: Some(prev_group.arrival.unwrap() + Duration::milliseconds(110)),
                };
                estimator.update(&prev_group, &group);
                prev_group = group;
            }
        };

        rising(&mut estimator, DEFAULT_SAMPLES_MAX_LEN * 2);
        assert!(
            estimator.estimate() > Duration::ZERO,
            "a rising delay should have produced an estimate to begin with"
        );

        estimator.expect_fast_rate_change();
        assert_eq!(estimator.samples.len(), 0);
        assert_eq!(estimator.accumulated_delay, Duration::ZERO);
        assert_eq!(estimator.estimate(), Duration::ZERO);

        // Still nothing to say until the window has refilled.
        rising(&mut estimator, DEFAULT_SAMPLES_MAX_LEN - 1);
        assert_eq!(estimator.estimate(), Duration::ZERO);

        rising(&mut estimator, 1);
        assert!(
            estimator.estimate() > Duration::ZERO,
            "the estimate should be back once the window is full again"
        );
    }

    // Helper to create fake PacketGroups with the desired `inter_group_delay`
    // for tests . The underlying absolute values are arbitrary since they don't
    // matter for our tests.
    fn with_inter_group_delay(inter_group_delay: Duration) -> (PacketGroup, PacketGroup) {
        let inter_departure_delay = Duration::milliseconds(100); // Exact value does not matter
        let inter_arrival_delay = inter_departure_delay + inter_group_delay;

        let prev_group = PacketGroup {
            packets: vec![],                             // Unused
            departure: Duration::milliseconds(1000),     // Exact value does not matter
            arrival: Some(Duration::milliseconds(1050)), // Exact value does not matter
        };

        let group = PacketGroup {
            packets: vec![], // Unused
            departure: prev_group.departure + inter_departure_delay,
            arrival: Some(prev_group.arrival.unwrap() + inter_arrival_delay),
        };

        (prev_group, group)
    }
}
