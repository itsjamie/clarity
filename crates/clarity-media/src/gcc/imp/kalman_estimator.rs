//! This is the estimator that follows the algorithm described in
//! https://datatracker.ietf.org/doc/html/draft-ietf-rmcat-gcc-02.

use super::Duration;
use super::EstimatorImpl;
use super::PacketGroup;

// Table1. Coefficient used for the measured noise variance
//  [0.1,0.001]
const CHI: f64 = 0.01;
const ONE_MINUS_CHI: f64 = 1. - CHI;

// Table1. State noise covariance matrix
const Q: f64 = 0.001;

// Table1. Initial value of the system error covariance
const INITIAL_ERROR_COVARIANCE: f64 = 0.1;

#[derive(Debug, PartialEq, Clone)]
pub struct KalmanEstimator {
    measure: Duration, // Delay variation measure
    gain: f64,
    measurement_uncertainty: f64, // var_v_hat(i-1)
    estimate_error: f64,          // e(i-1)
    estimate: Duration,           // m_hat(i-1)
}

impl Default for KalmanEstimator {
    fn default() -> Self {
        Self {
            measure: Duration::ZERO,
            gain: 0.,
            measurement_uncertainty: 0.,
            estimate_error: INITIAL_ERROR_COVARIANCE,
            estimate: Duration::ZERO,
        }
    }
}

impl EstimatorImpl for KalmanEstimator {
    fn update(&mut self, prev_group: &PacketGroup, group: &PacketGroup) {
        self.measure = group.inter_delay_variation(prev_group);

        let z = self.measure - self.estimate;
        let zms = z.whole_microseconds() as f64 / 1000.0;

        // This doesn't exactly follows the spec as we should compute and
        // use f_max here, no implementation we have found actually uses it.
        let alpha = ONE_MINUS_CHI.powf(30.0 / (1000. * 5. * 1_000_000.));
        let root = self.measurement_uncertainty.sqrt();
        let root3 = 3. * root;

        if zms > root3 {
            self.measurement_uncertainty =
                (alpha * self.measurement_uncertainty + (1. - alpha) * root3.powf(2.)).max(1.);
        } else {
            self.measurement_uncertainty =
                (alpha * self.measurement_uncertainty + (1. - alpha) * zms.powf(2.)).max(1.);
        }

        let estimate_uncertainty = self.estimate_error + Q;
        self.gain = estimate_uncertainty / (estimate_uncertainty + self.measurement_uncertainty);
        self.estimate += Duration::nanoseconds((self.gain * zms * 1_000_000.) as i64);
        self.estimate_error = (1. - self.gain) * estimate_uncertainty;
    }

    fn estimate(&self) -> Duration {
        self.estimate
    }

    fn measure(&self) -> Duration {
        self.measure
    }

    fn expect_fast_rate_change(&mut self) {
        // `estimate_error` is the system error covariance `e(i-1)`, which the
        // gain is computed from: the larger it is, the more the next
        // measurements weigh against the current estimate. Q is the state
        // noise covariance. This is libwebrtc's
        // `E_[1][1] += 10 * process_noise_[1]` from `OveruseEstimator::Update`,
        // one state variable instead of two.
        self.estimate_error += 10. * Q;
    }
}

#[cfg(test)]
mod tests {
    use super::super::PacketGroup;
    use super::*;

    /// Widening the covariance is what makes the filter believe the next
    /// measurements over the estimate it settled on before.
    #[test]
    fn expect_fast_rate_change_weighs_the_next_measurement_higher() {
        let mut settled = KalmanEstimator::default();
        let (prev_group, quiet) = with_inter_group_delay(Duration::ZERO);
        for _ in 0..50 {
            settled.update(&prev_group, &quiet);
        }

        let mut widened = settled.clone();
        widened.expect_fast_rate_change();

        // The same delay variation, seen by both.
        let (prev_group, delayed) = with_inter_group_delay(Duration::milliseconds(10));
        settled.update(&prev_group, &delayed);
        widened.update(&prev_group, &delayed);

        assert!(
            widened.estimate() > settled.estimate(),
            "the widened filter should have moved further on the same \
             measurement, moved {} against {}",
            widened.estimate(),
            settled.estimate(),
        );
    }

    // Two groups whose delay variation is `inter_group_delay`. The absolute
    // values do not matter: the estimator only reads the variation.
    fn with_inter_group_delay(inter_group_delay: Duration) -> (PacketGroup, PacketGroup) {
        let inter_departure_delay = Duration::milliseconds(100);

        let prev_group = PacketGroup {
            packets: vec![],
            departure: Duration::milliseconds(1000),
            arrival: Some(Duration::milliseconds(1050)),
        };

        let group = PacketGroup {
            packets: vec![],
            departure: prev_group.departure + inter_departure_delay,
            arrival: Some(prev_group.arrival.unwrap() + inter_departure_delay + inter_group_delay),
        };

        (prev_group, group)
    }
}
