/**
 * element-claritygccbwe:
 *
 * Implements the [Google Congestion Control algorithm](https://datatracker.ietf.org/doc/html/draft-ietf-rmcat-gcc-02).
 *
 * This element should always be placed right before a `rtpsession` and will only work
 * when [twcc](https://datatracker.ietf.org/doc/html/draft-holmer-rmcat-transport-wide-cc-extensions-01) is enabled
 * as the bandwidth estimation relies on it.
 *
 * This element implements the pacing as describe in the spec by running its
 * own streaming thread on its srcpad. It implements the mathematic as closely
 * to the specs as possible and sets the #claritygccbwe:estimated-bitrate property
 * each time a new estimate is produced. User should connect to the
 * `claritygccbwe::notify::estimated-bitrate` signal to make the encoders target
 * that new estimated bitrate (the overall target bitrate of the potentially
 * multiple encoders should match that target bitrate, the application is
 * responsible for determining what bitrate to give to each encode)
 *
 */
use gstreamer as gst;

use gst::{glib, prelude::*, subclass::prelude::*};
use smallvec::SmallVec;
use std::sync::LazyLock;
use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    fmt::Debug,
    mem,
    sync::Mutex,
    time::Instant,
};
use time::Duration;

type Bitrate = u32;
type BufferList = SmallVec<[gst::Buffer; 10]>;

const DEFAULT_MIN_BITRATE: Bitrate = 1000;
const DEFAULT_ESTIMATED_BITRATE: Bitrate = 2_048_000;
const DEFAULT_MAX_BITRATE: Bitrate = 8_192_000;
const DEFAULT_PACING_FACTOR: f64 = 1.0;

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        "claritygccbwe",
        gst::DebugColorFlags::empty(),
        Some("Google Congestion Controller based bandwidth estimator"),
    )
});

// Table1. Time limit in milliseconds  between packet bursts which  identifies a group
const BURST_TIME: Duration = Duration::milliseconds(5);

// Table1. Initial value for the adaptive threshold
const INITIAL_DEL_VAR_TH: Duration = Duration::microseconds(12500);

// Table1. Time required to trigger an overuse signal
const OVERUSE_TIME_TH: Duration = Duration::milliseconds(10);

// from 5.5 "beta is typically chosen to be in the interval [0.8, 0.95], 0.85 is the RECOMMENDED value."
const BETA: f64 = 0.85;

// From "5.5 Rate control" It is RECOMMENDED to measure this average and
// standard deviation with an exponential moving average with the smoothing
// factor 0.5 (NOTE: the spec mentions 0.95 here but in the equations it is 0.5
// and other implementations use 0.5), as it is expected that this average
// covers multiple occasions at which we are in the Decrease state.
const MOVING_AVERAGE_SMOOTHING_FACTOR: f64 = 0.5;

// The variance of the link capacity estimate is kept normalized by the
// estimate itself and expressed in kbit/s, then clamped, exactly as
// libwebrtc's `LinkCapacityEstimator::Update` does. The floor is what makes
// the closeness range usable: with the plain variance of the samples a single
// sample has a standard deviation of 0, so `avg - 3*sigma .. avg + 3*sigma`
// is the empty range and nothing is ever close to the capacity we know about.
// 0.4 ~= 14 kbit/s at 500 kbit/s, 2.5 ~= 35 kbit/s at 500 kbit/s.
const MIN_LINK_CAPACITY_DEVIATION: f64 = 0.4;
const MAX_LINK_CAPACITY_DEVIATION: f64 = 2.5;

// `N(i)` is the number of packets received the past T seconds and `L(j)` is
// the payload size of packet j.  A window between 0.5 and 1 second is
// RECOMMENDED.
const PACKETS_RECEIVED_WINDOW: Duration = Duration::milliseconds(1000); // ms

// from "5.4 Over-use detector" ->
// Moreover, del_var_th(i) SHOULD NOT be updated if this condition
// holds:
//
// ```
// |m(i)| - del_var_th(i) > 15
// ```
const MAX_M_MINUS_DEL_VAR_TH: Duration = Duration::milliseconds(15);

// from 5.4 "It is also RECOMMENDED to clamp del_var_th(i) to the range [6, 600]"
const MIN_THRESHOLD: Duration = Duration::milliseconds(6);
const MAX_THRESHOLD: Duration = Duration::milliseconds(600);

// From 5.5 ""Close" is defined as three standard deviations around this average"
const STANDARD_DEVIATION_CLOSE_NUM: f64 = 3.;

// Minimal duration between 2 updates on the lost based rate controller
const LOSS_UPDATE_INTERVAL: Duration = Duration::milliseconds(200);
const LOSS_DECREASE_THRESHOLD: f64 = 0.1;
const LOSS_INCREASE_THRESHOLD: f64 = 0.02;
const LOSS_INCREASE_FACTOR: f64 = 1.05;

// Minimal duration between 2 updates on the lost based rate controller
const DELAY_UPDATE_INTERVAL: Duration = Duration::milliseconds(100);

const ROUND_TRIP_TIME_WINDOW_SIZE: usize = 100;

// Window over which the rate the element actually sends at is measured, the
// input to the application-limited region (ALR) detection below. Same order
// as the window libwebrtc's `AlrDetector` measures the pacer's output over.
const SENT_RATE_WINDOW: Duration = Duration::milliseconds(500);

// Ignore the sent rate until the element has been running for at least this
// long, so that the first feedback batches are not judged against a window
// that is mostly empty because it predates the element. libwebrtc's
// `RateStatistics::Rate` reports no rate under the same condition.
const MIN_SENT_RATE_WINDOW: Duration = Duration::milliseconds(50);

// The element is application limited when it sends less than
// ALR_ENTER_RATIO of the bitrate it estimated, and stops being so once it
// sends ALR_EXIT_RATIO of it again. The gap between the two is hysteresis:
// an encoder's output swings from frame to frame, and a single quiet frame
// must not flip the state.
const ALR_ENTER_RATIO: f64 = 0.70;
const ALR_EXIT_RATIO: f64 = 0.75;

// All wall-clock reads inside the estimator go through `now()` (and the RTT
// reference in `Detector::update_rtts`); in test builds they are redirected to
// the manually advanced clock in `testing::clock` so scenarios can drive the
// element deterministically without sleeping.
fn now() -> Instant {
    #[cfg(test)]
    {
        testing::clock::instant()
    }
    #[cfg(not(test))]
    {
        Instant::now()
    }
}

const fn ts2dur(t: gst::ClockTime) -> Duration {
    Duration::nanoseconds(t.nseconds() as i64)
}

const fn dur2ts(t: Duration) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(t.whole_nanoseconds() as u64)
}

#[derive(Debug)]
enum BandwidthEstimationOp {
    /// Don't update target bitrate
    Hold,
    /// Decrease target bitrate
    #[allow(unused)]
    Decrease(String /* reason */),
    #[allow(unused)]
    Increase(String /* reason */),
}

#[derive(Debug, Clone, Copy)]
enum ControllerType {
    // Running the "delay-based controller"
    Delay,
    // Running the "loss based controller"
    Loss,
}

#[derive(Debug, Clone, Copy)]
struct Packet {
    departure: Duration,
    arrival: Duration,
    size: usize,
    seqnum: u64,
}

fn human_kbits<T: Into<f64>>(bits: T) -> String {
    format!("{:.2}kb", (bits.into() / 1_000.))
}

impl Packet {
    fn from_structure(structure: &gst::StructureRef) -> Option<Self> {
        let lost = structure.get::<bool>("lost").unwrap();
        let departure = match structure.get::<gst::ClockTime>("local-ts") {
            Err(e) => {
                gst::fixme!(
                    CAT,
                    "Got packet feedback without local-ts: {:?} - what does that mean?",
                    e
                );
                return None;
            }
            Ok(ts) => ts,
        };

        let seqnum = structure.get::<u32>("seqnum").unwrap() as u64;
        if lost {
            return Some(Packet {
                arrival: Duration::ZERO,
                departure: ts2dur(departure),
                size: structure.get::<u32>("size").unwrap() as usize,
                seqnum,
            });
        }

        let arrival = structure.get::<gst::ClockTime>("remote-ts").unwrap();

        Some(Packet {
            arrival: ts2dur(arrival),
            departure: ts2dur(departure),
            size: structure.get::<u32>("size").unwrap() as usize,
            seqnum,
        })
    }
}

#[derive(Clone)]
struct PacketGroup {
    packets: Vec<Packet>,
    departure: Duration,       // ms
    arrival: Option<Duration>, // ms
}

impl Default for PacketGroup {
    fn default() -> Self {
        Self {
            packets: Default::default(),
            departure: Duration::ZERO,
            arrival: None,
        }
    }
}

impl PacketGroup {
    fn add(&mut self, packet: Packet) {
        if self.departure.is_zero() {
            self.departure = packet.departure;
        }

        self.arrival = Some(
            self.arrival
                .map_or_else(|| packet.arrival, |v| Duration::max(v, packet.arrival)),
        );
        self.packets.push(packet);
    }

    /// Returns the delta between self.arrival_time and @prev_group.arrival_time in ms
    // t(i) - t(i-1)
    fn inter_arrival_time(&self, prev_group: &Self) -> Duration {
        // Should never be called if we haven't gotten feedback for all
        // contained packets
        self.arrival.unwrap() - prev_group.arrival.unwrap()
    }

    fn inter_arrival_time_pkt(&self, next_pkt: &Packet) -> Duration {
        // Should never be called if we haven't gotten feedback for all
        // contained packets
        next_pkt.arrival - self.arrival.unwrap()
    }

    /// Returns the delta between self.departure_time and @prev_group.departure_time in ms
    // T(i) - T(i-1)
    fn inter_departure_time(&self, prev_group: &Self) -> Duration {
        // Should never be called if we haven't gotten feedback for all
        // contained packets
        self.departure - prev_group.departure
    }

    fn inter_departure_time_pkt(&self, next_pkt: &Packet) -> Duration {
        // Should never be called if we haven't gotten feedback for all
        // contained packets
        next_pkt.departure - self.departure
    }

    /// Returns the delta between intern arrival time and inter departure time in ms
    fn inter_delay_variation(&self, prev_group: &Self) -> Duration {
        // Should never be called if we haven't gotten feedback for all
        // contained packets
        self.inter_arrival_time(prev_group) - self.inter_departure_time(prev_group)
    }

    fn inter_delay_variation_pkt(&self, next_pkt: &Packet) -> Duration {
        // Should never be called if we haven't gotten feedback for all
        // contained packets
        self.inter_arrival_time_pkt(next_pkt) - self.inter_departure_time_pkt(next_pkt)
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
enum NetworkUsage {
    Normal,
    Over,
    Under,
}

/// Simple abstraction over an estimator that allows different estimator
/// implementations, and allows them to be changed at runtime.
trait EstimatorImpl: Send {
    /// Update the estimator.
    fn update(&mut self, prev_group: &PacketGroup, group: &PacketGroup);

    /// Get the estimate that will be compared against the dynamic delay
    /// threshold of GCC. Note that this value will be multiplied by a dynamic
    /// factor before being compared against the threshold.
    fn estimate(&self) -> Duration;

    /// Get the most recent measurement used as input to the estimator.
    /// Typically this will be the most recent inter-group delay variation.
    fn measure(&self) -> Duration;

    /// Widen the estimator's uncertainty because the measurements it has
    /// been fed no longer describe the conditions it is about to see, so it
    /// should move fast on the next few of them.
    ///
    /// libwebrtc does the same thing in two places. `OveruseEstimator::Update`
    /// inflates the offset state's variance (`E_[1][1] += 10 *
    /// process_noise_[1]`) whenever the network hypothesis changes, and
    /// `DelayBasedBwe::IncomingPacketFeedbackVector` drops and rebuilds its
    /// `TrendlineEstimator` when a stream has been quiet, rather than letting
    /// the samples from before the silence set the slope.
    fn expect_fast_rate_change(&mut self);
}

mod kalman_estimator;
use kalman_estimator::KalmanEstimator;

mod linear_regression_estimator;
use linear_regression_estimator::LinearRegressionEstimator;

/// An enum will all known estimators. The active estimator can be changed at
/// runtime through the "estimator" property.
#[derive(Debug, Default, Copy, Clone, glib::Enum)]
#[repr(i32)]
#[enum_type(name = "GstClarityGCCBwEEstimator")]
pub enum Estimator {
    #[default]
    #[enum_value(name = "Use Kalman filter")]
    Kalman = 0,
    #[enum_value(name = "Use linear regression slope")]
    LinearRegression = 1,
}

impl Estimator {
    fn to_impl(self) -> Box<dyn EstimatorImpl> {
        match self {
            Estimator::Kalman => Box::<KalmanEstimator>::default(),
            Estimator::LinearRegression => Box::<LinearRegressionEstimator>::default(),
        }
    }
}

struct Detector {
    group: PacketGroup,              // Packet group that is being filled
    prev_group: Option<PacketGroup>, // Group that is ready to be used once "group" is filled

    last_received_packets: BTreeMap<u64, Packet>, // Order by seqnums, front is the newest, back is the oldest

    // Last loss update
    last_loss_update: Option<Instant>,
    // Moving average of the packet loss
    loss_average: f64,

    // Estimator fields
    estimator_impl: Box<dyn EstimatorImpl>,

    // Threshold fields
    threshold: Duration,
    last_threshold_update: Option<Instant>,
    num_deltas: i64,

    // Overuse related fields
    increasing_counter: u32,
    last_overuse_estimate: Duration,
    last_use_detector_update: Instant,
    increasing_duration: Duration,

    // round-trip-time estimations
    rtts: VecDeque<Duration>,
    // Unused in test builds, where `update_rtts` reads the rig's clock instead
    #[cfg_attr(test, allow(dead_code))]
    clock: gst::Clock,

    // Current network usage state
    usage: NetworkUsage,

    twcc_extended_seqnum: u64,
}

// Monitors packet loss and network overuse through because of delay
impl Debug for Detector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Network Usage: {:?}. Effective bitrate: {}ps - Measure: {} Estimate: {} threshold {} - overuse_estimate {}",
            self.usage,
            human_kbits(self.effective_bitrate()),
            self.estimator_impl.measure(),
            self.estimator_impl.estimate(),
            self.threshold,
            self.last_overuse_estimate,
        )
    }
}

impl Detector {
    fn new(estimator: Estimator) -> Self {
        Detector {
            group: Default::default(),
            prev_group: Default::default(),

            /* Smallish value to hold PACKETS_RECEIVED_WINDOW packets */
            last_received_packets: BTreeMap::new(),

            last_loss_update: None,
            loss_average: 0.,

            estimator_impl: estimator.to_impl(),

            threshold: INITIAL_DEL_VAR_TH,
            last_threshold_update: None,
            num_deltas: 0,

            last_use_detector_update: now(),
            increasing_counter: 0,
            last_overuse_estimate: Duration::ZERO,
            increasing_duration: Duration::ZERO,

            rtts: Default::default(),
            clock: gst::SystemClock::obtain(),

            usage: NetworkUsage::Normal,

            twcc_extended_seqnum: 0,
        }
    }

    fn loss_ratio(&self) -> f64 {
        self.loss_average
    }

    fn update_last_received_packets(&mut self, packet: Packet) {
        self.last_received_packets.insert(packet.seqnum, packet);
        self.evict_old_received_packets();
    }

    fn evict_old_received_packets(&mut self) {
        let last_arrival = self.newest_packet_in_window_ts();
        while last_arrival > self.oldest_packet_in_window_ts()
            && last_arrival - self.oldest_packet_in_window_ts() > PACKETS_RECEIVED_WINDOW
        {
            let oldest_seqnum = *self.last_received_packets.iter().next().unwrap().0;
            self.last_received_packets.remove(&oldest_seqnum);
        }
        // In the case of a possible wraparound due to reference time exceeding 24 bits
        // remove values that are chronologically incorrect. This *shouldn't* happen, but
        // if the input data contains a wraparound, memory will start leaking otherwise.
        // Simply checking the size of the map won't work, as the data accumulated will then
        // be incorrect; the map has to be wiped when such data arrives.
        // The value of whole_days() is picked to simply be very high. If the timestamp jumps
        // 24 hours then an error must have occured. It could have been any very large number.
        while last_arrival < self.oldest_packet_in_window_ts()
            && (self.oldest_packet_in_window_ts() - last_arrival).whole_days() > 0
        {
            let oldest_seqnum = *self.last_received_packets.iter().next().unwrap().0;
            self.last_received_packets.remove(&oldest_seqnum);
        }
    }

    fn newest_packet_in_window_ts(&self) -> Duration {
        self.last_received_packets
            .iter()
            .next_back()
            .unwrap()
            .1
            .arrival
    }

    /// Returns the effective received bitrate during the last PACKETS_RECEIVED_WINDOW
    fn effective_bitrate(&self) -> Bitrate {
        if self.last_received_packets.is_empty() {
            return 0;
        }

        let duration = self
            .last_received_packets
            .iter()
            .next_back()
            .unwrap()
            .1
            .arrival
            - self.last_received_packets.iter().next().unwrap().1.arrival;
        let bits = self
            .last_received_packets
            .values()
            .map(|p| p.size as f64)
            .sum::<f64>()
            * 8.;

        (bits / (duration.whole_nanoseconds() as f64 / gst::ClockTime::SECOND.nseconds() as f64))
            as Bitrate
    }

    fn oldest_packet_in_window_ts(&self) -> Duration {
        self.last_received_packets.iter().next().unwrap().1.arrival
    }

    fn update_rtts(&mut self, packets: &Vec<Packet>) {
        let mut rtt = Duration::nanoseconds(i64::MAX);
        #[cfg(test)]
        let now = testing::clock::stream();
        #[cfg(not(test))]
        let now = ts2dur(self.clock.time());
        for packet in packets {
            rtt = (now - packet.departure).min(rtt);
        }

        self.rtts.push_back(rtt);
        if self.rtts.len() > ROUND_TRIP_TIME_WINDOW_SIZE {
            self.rtts.pop_front();
        }
    }

    fn rtt(&self) -> Duration {
        Duration::nanoseconds(
            (self
                .rtts
                .iter()
                .map(|d| d.whole_nanoseconds() as f64)
                .sum::<f64>()
                / self.rtts.len() as f64) as i64,
        )
    }

    fn update(&mut self, packets: &mut Vec<Packet>) {
        self.update_rtts(packets);
        let mut lost_packets = 0.;
        let n_packets = packets.len();
        for pkt in packets {
            // We know feedbacks packets will arrive "soon" after the packets they are reported for or considered
            // lost so we can make the assumption that
            let mut seqnum = pkt.seqnum + (self.twcc_extended_seqnum & !0xffff_u64);

            if seqnum < self.twcc_extended_seqnum {
                let diff = self.twcc_extended_seqnum.overflowing_sub(seqnum).0;

                if diff > i16::MAX as u64 {
                    seqnum += 1 << 16;
                }
            } else {
                let diff = seqnum.overflowing_sub(self.twcc_extended_seqnum).0;

                if diff > i16::MAX as u64 {
                    if seqnum < 1 << 16 {
                        eprintln!(
                            "Cannot unwrap, any wrapping took place yet. Returning 0 without updating extended timestamp."
                        );
                    } else {
                        seqnum -= 1 << 16;
                    }
                }
            }

            self.twcc_extended_seqnum = u64::max(seqnum, self.twcc_extended_seqnum);

            pkt.seqnum = seqnum;

            if pkt.arrival.is_zero() {
                lost_packets += 1.;
                continue;
            }

            self.update_last_received_packets(*pkt);

            if self.group.arrival.is_none() {
                self.group.add(*pkt);

                continue;
            }

            if pkt.arrival < self.group.arrival.unwrap() {
                // ignore out of order arrivals
                continue;
            }

            if pkt.departure >= self.group.departure {
                if self.group.inter_departure_time_pkt(pkt) < BURST_TIME {
                    self.group.add(*pkt);
                    continue;
                }

                // 5.2 Pre-filtering
                //
                // A Packet which has an inter-arrival time less than burst_time and
                // an inter-group delay variation d(i) less than 0 is considered
                // being part of the current group of packets.
                if self.group.inter_arrival_time_pkt(pkt) < BURST_TIME
                    && self.group.inter_delay_variation_pkt(pkt) < Duration::ZERO
                {
                    self.group.add(*pkt);
                    continue;
                }

                let group = mem::take(&mut self.group);
                gst::trace!(
                    CAT,
                    "Packet group done: {:?}",
                    gst::ClockTime::from_nseconds(group.departure.whole_nanoseconds() as u64)
                );
                if let Some(prev_group) = self.prev_group.replace(group.clone()) {
                    // 5.3 Arrival-time filter
                    self.estimator_impl.update(&prev_group, &group);
                    // 5.4 Over-use detector
                    self.overuse_filter();
                }
            } else {
                gst::debug!(
                    CAT,
                    "Ignoring packet departed at {:?} as we got feedback too late",
                    gst::ClockTime::from_nseconds(pkt.departure.whole_nanoseconds() as u64)
                );
            }
        }

        self.compute_loss_average(lost_packets / n_packets as f64);
    }

    fn compute_loss_average(&mut self, loss_fraction: f64) {
        let now = now();

        if let Some(ref last_update) = self.last_loss_update {
            self.loss_average = loss_fraction
                + (-Duration::try_from(now - *last_update)
                    .unwrap()
                    .whole_milliseconds() as f64)
                    .exp()
                    * (self.loss_average - loss_fraction);
        }

        self.last_loss_update = Some(now);
    }

    fn compare_threshold(&mut self) -> (NetworkUsage, Duration) {
        // FIXME: It is unclear where that factor is coming from but all
        // implementations we found have it (libwebrtc, pion, jitsi...), and the
        // algorithm does not work without it.
        const MAX_DELTAS: i64 = 60;

        self.num_deltas += 1;
        if self.num_deltas < 2 {
            return (NetworkUsage::Normal, self.estimator_impl.estimate());
        }

        let amplified_estimate = Duration::nanoseconds(
            self.estimator_impl.estimate().whole_nanoseconds() as i64
                * i64::min(self.num_deltas, MAX_DELTAS),
        );
        let usage = if amplified_estimate > self.threshold {
            NetworkUsage::Over
        } else if amplified_estimate.whole_nanoseconds() < -self.threshold.whole_nanoseconds() {
            NetworkUsage::Under
        } else {
            NetworkUsage::Normal
        };

        self.update_threshold(&amplified_estimate);

        (usage, amplified_estimate)
    }

    fn update_threshold(&mut self, estimate: &Duration) {
        const K_U: f64 = 0.01; // Table1. Coefficient for the adaptive threshold
        const K_D: f64 = 0.00018; // Table1. Coefficient for the adaptive threshold
        const MAX_TIME_DELTA: Duration = Duration::milliseconds(100);

        let now = now();
        if self.last_threshold_update.is_none() {
            self.last_threshold_update = Some(now);
        }

        let abs_estimate = estimate.abs();
        if abs_estimate > self.threshold + MAX_M_MINUS_DEL_VAR_TH {
            self.last_threshold_update = Some(now);
            return;
        }

        let k = if abs_estimate < self.threshold {
            K_D
        } else {
            K_U
        };
        let time_delta = Duration::try_from(now - self.last_threshold_update.unwrap())
            .unwrap()
            .min(MAX_TIME_DELTA);
        let d = abs_estimate - self.threshold;
        let add = k * d.whole_milliseconds() as f64 * time_delta.whole_milliseconds() as f64;

        self.threshold += Duration::nanoseconds((add * 100. * 1_000.) as i64);
        self.threshold = self.threshold.clamp(MIN_THRESHOLD, MAX_THRESHOLD);
        self.last_threshold_update = Some(now);
    }

    fn overuse_filter(&mut self) {
        let (th_usage, amplified_estimate) = self.compare_threshold();

        let now = now();
        let delta = now - self.last_use_detector_update;
        self.last_use_detector_update = now;
        match th_usage {
            NetworkUsage::Over => {
                self.increasing_duration += delta;
                self.increasing_counter += 1;

                if self.increasing_duration > OVERUSE_TIME_TH
                    && self.increasing_counter > 1
                    && amplified_estimate > self.last_overuse_estimate
                {
                    self.usage = NetworkUsage::Over;
                }
            }
            NetworkUsage::Under | NetworkUsage::Normal => {
                self.increasing_duration = Duration::ZERO;
                self.increasing_counter = 0;

                self.usage = th_usage;
            }
        }
        gst::log!(
            CAT,
            "{:?} - measure: {} - estimate: {} - amp_est: {} - th: {} - inc_dur: {} - inc_cnt: {}",
            th_usage,
            self.estimator_impl.measure(),
            self.estimator_impl.estimate(),
            amplified_estimate,
            self.threshold,
            self.increasing_duration,
            self.increasing_counter,
        );
        self.last_overuse_estimate = amplified_estimate;
    }
}

/// What the link was measured to carry, as an exponential moving average of
/// the effective bitrate sampled whenever the delay based controller
/// decreases, plus the spread of those samples. The band it defines is what
/// tells "we are around the capacity we know about", where the rate control
/// creeps up additively, from "we are nowhere near it", where it ramps
/// multiplicatively to find the capacity again.
///
/// Ported from libwebrtc's `LinkCapacityEstimator`, which `AimdRateControl`
/// uses for the same decision, with two deliberate divergences: the smoothing
/// factor stays the one this element already used for the average (0.5, from
/// section 5.5 of the spec) rather than libwebrtc's 0.05, and samples taken
/// from probes are not a thing here, so the only input is an overuse.
#[derive(Default, Debug)]
struct LinkCapacity {
    /// The average of the samples, in bit/s.
    average: Option<f64>,
    /// The variance of `average`, normalized by it and expressed in kbit/s so
    /// that the clamp above is meaningful whatever the bitrate.
    normalized_variance: f64,
}

impl LinkCapacity {
    fn update<T: Into<f64>>(&mut self, value: T) {
        let sample = value.into();
        let average = match self.average {
            Some(avg) => avg + MOVING_AVERAGE_SMOOTHING_FACTOR * (sample - avg),
            None => sample,
        };
        self.average = Some(average);

        let error_kbits = (average - sample) / 1000.;
        let norm = f64::max(average / 1000., 1.);
        self.normalized_variance = ((1. - MOVING_AVERAGE_SMOOTHING_FACTOR)
            * self.normalized_variance
            + MOVING_AVERAGE_SMOOTHING_FACTOR * error_kbits * error_kbits / norm)
            .clamp(MIN_LINK_CAPACITY_DEVIATION, MAX_LINK_CAPACITY_DEVIATION);
    }

    /// Forgets the capacity, so that the next sample is adopted as-is instead
    /// of being smoothed against measurements the link has moved away from.
    fn reset(&mut self) {
        *self = Default::default();
    }

    /// Half the width of the band, in bit/s: "close" is defined by the spec
    /// as three standard deviations around the average.
    fn closeness(&self, average: f64) -> f64 {
        STANDARD_DEVIATION_CLOSE_NUM * 1000. * (self.normalized_variance * average / 1000.).sqrt()
    }

    /// Whether `value` sits inside the band around the measured capacity.
    /// Always false while nothing has been measured, which is what makes a
    /// fresh element ramp up multiplicatively.
    fn is_close(&self, value: Bitrate) -> bool {
        self.average.is_some_and(|avg| {
            let closeness = self.closeness(avg);
            ((avg - closeness)..(avg + closeness)).contains(&(value as f64))
        })
    }

    /// Whether `value` is above the band, i.e. the link carries more than
    /// what we remember of it.
    fn is_above(&self, value: Bitrate) -> bool {
        self.average
            .is_some_and(|avg| value as f64 > avg + self.closeness(avg))
    }

    /// Whether `value` is below the band, i.e. the link carries less than
    /// what we remember of it.
    fn is_below(&self, value: Bitrate) -> bool {
        self.average
            .is_some_and(|avg| (value as f64) < avg - self.closeness(avg))
    }

    #[cfg(test)]
    fn band(&self) -> Option<(Bitrate, Bitrate)> {
        self.average.map(|avg| {
            let closeness = self.closeness(avg);
            (
                f64::max(avg - closeness, 0.) as Bitrate,
                (avg + closeness) as Bitrate,
            )
        })
    }
}

struct State {
    /// Note: The target bitrate applied is the min of
    /// target_bitrate_on_delay and target_bitrate_on_loss
    estimated_bitrate: Bitrate,

    /// Bitrate target based on delay factor for all video streams.
    /// Hasn't been tested with multiple video streams, but
    /// current design is simply to divide bitrate equally.
    target_bitrate_on_delay: Bitrate,

    /// Used in additive mode to track last control time, influences
    /// calculation of added value according to gcc section 5.5
    last_increase_on_delay: Option<Instant>,
    last_decrease_on_delay: Instant,

    /// Bitrate target based on loss for all video streams.
    target_bitrate_on_loss: Bitrate,

    last_increase_on_loss: Instant,
    last_decrease_on_loss: Instant,

    /// What the link has been measured to carry, updated when the bitrate is
    /// decreased. Never fed from an application-limited period: the decrease
    /// is held there (see `delay_control`), and the rate measured then is the
    /// rate of the content we happened to have, not of the link.
    link_capacity: LinkCapacity,

    last_control_op: BandwidthEstimationOp,

    min_bitrate: Bitrate,
    max_bitrate: Bitrate,

    estimator: Estimator,
    detector: Detector,

    clock_entry: Option<gst::SingleShotClockId>,

    // Implemented like a leaky bucket
    buffers: VecDeque<gst::Buffer>,
    // Number of bits remaining from previous burst
    budget_offset: i64,
    // Multiple of estimated_bitrate at which the leaky bucket drains
    pacing_factor: f64,

    /// Bytes handed to the srcpad by the pacer, with the time they left,
    /// kept over SENT_RATE_WINDOW. Every buffer goes through the pacer, so
    /// this is the rate the element is actually sending at.
    sent_bytes: VecDeque<(Instant, usize)>,
    /// When the counter above started, so that a window that predates the
    /// element is not mistaken for an idle one.
    sent_window_start: Instant,
    /// Whether the element is sending less than the estimate allows, i.e.
    /// the content, not the link, is what is limiting it.
    application_limited: bool,

    /// Test instrumentation only: counts calls to `create_buffer_list` where
    /// the 30ms cap forced eviction beyond the normal drain budget. Read by
    /// the pacing-factor scenario test; has no effect on production builds.
    #[cfg(test)]
    force_leak_count: usize,

    flow_return: Result<gst::FlowSuccess, gst::FlowError>,
    last_push: Instant,
}

impl Default for State {
    fn default() -> Self {
        let estimator = Estimator::default();
        Self {
            target_bitrate_on_delay: DEFAULT_ESTIMATED_BITRATE,
            target_bitrate_on_loss: DEFAULT_ESTIMATED_BITRATE,
            last_increase_on_loss: now(),
            last_decrease_on_loss: now(),
            link_capacity: Default::default(),
            last_increase_on_delay: None,
            last_decrease_on_delay: now(),
            min_bitrate: DEFAULT_MIN_BITRATE,
            max_bitrate: DEFAULT_MAX_BITRATE,
            estimator,
            detector: Detector::new(estimator),
            buffers: Default::default(),
            estimated_bitrate: DEFAULT_ESTIMATED_BITRATE,
            last_control_op: BandwidthEstimationOp::Increase("Initial increase".into()),
            flow_return: Err(gst::FlowError::Flushing),
            clock_entry: None,
            last_push: now(),
            budget_offset: 0,
            pacing_factor: DEFAULT_PACING_FACTOR,
            sent_bytes: Default::default(),
            sent_window_start: now(),
            application_limited: false,
            #[cfg(test)]
            force_leak_count: 0,
        }
    }
}

impl State {
    // 4. sending engine implementing a "leaky bucket"
    fn create_buffer_list(&mut self, bwe: &super::BandwidthEstimator) -> BufferList {
        let now = now();
        let elapsed = Duration::try_from(now - self.last_push).unwrap();
        // The pacer drains at pacing_factor * estimated_bitrate rather than
        // at estimated_bitrate itself; with the default factor of 1.0 this
        // is exactly the estimated bitrate, unchanged from before.
        let pacing_bitrate = self.estimated_bitrate as f64 * self.pacing_factor;
        let mut budget = (elapsed.whole_nanoseconds() as i64)
            .mul_div_round(
                pacing_bitrate as i64,
                gst::ClockTime::SECOND.nseconds() as i64,
            )
            .unwrap()
            + self.budget_offset;
        let total_budget = budget;
        let mut remaining = self.buffers.iter().map(|b| b.size() as f64).sum::<f64>() * 8.;
        let total_size = remaining;

        let mut list_size = 0;
        let mut list = BufferList::new();

        // Leak the bucket so it can hold at most 30ms of data at the pacing rate
        let maximum_remaining_bits = 30. * pacing_bitrate / 1000.;
        let mut leaked = false;
        while (budget > 0 || remaining > maximum_remaining_bits) && !self.buffers.is_empty() {
            let buf = self.buffers.pop_back().unwrap();
            let n_bits = buf.size() * 8;

            leaked = budget <= 0 && remaining > maximum_remaining_bits;
            list_size += buf.size();
            list.push(buf);
            budget -= n_bits as i64;
            remaining -= n_bits as f64;
        }

        gst::trace!(
            CAT,
            obj = bwe,
            "{} bitrate: {}ps budget: {}/{} sending: {} Remaining: {}/{}",
            elapsed,
            human_kbits(self.estimated_bitrate),
            human_kbits(budget as f64),
            human_kbits(total_budget as f64),
            human_kbits(list_size as f64 * 8.),
            human_kbits(remaining),
            human_kbits(total_size)
        );

        self.last_push = now;
        self.budget_offset = if !leaked { budget } else { 0 };
        self.account_sent(now, list_size);
        #[cfg(test)]
        if leaked {
            self.force_leak_count += 1;
        }

        list
    }

    /// Records bytes leaving the element, and forgets the ones that fell out
    /// of SENT_RATE_WINDOW. Called on every pacer run, including the ones
    /// that send nothing, so the window empties out during silence.
    fn account_sent(&mut self, now: Instant, bytes: usize) {
        if bytes > 0 {
            self.sent_bytes.push_back((now, bytes));
        }

        while let Some((ts, _)) = self.sent_bytes.front() {
            if now.duration_since(*ts) <= SENT_RATE_WINDOW {
                break;
            }
            self.sent_bytes.pop_front();
        }
    }

    /// The rate the element sent at over the last SENT_RATE_WINDOW, in
    /// bit/sec, or `None` while the window is too young to say.
    fn sent_bitrate(&self, now: Instant) -> Option<f64> {
        let window = Duration::try_from(now - self.sent_window_start)
            .unwrap()
            .min(SENT_RATE_WINDOW);
        if window < MIN_SENT_RATE_WINDOW {
            return None;
        }

        let bits = self
            .sent_bytes
            .iter()
            .filter(|(ts, _)| now.duration_since(*ts) <= SENT_RATE_WINDOW)
            .map(|(_, bytes)| *bytes as f64)
            .sum::<f64>()
            * 8.;

        Some(bits / window.as_seconds_f64())
    }

    /// Recomputes whether the element is application limited, on each batch
    /// of feedback, which is the cadence the rate controllers run at.
    /// Returns whether that changed, leaving it to the caller to notify
    /// about it once it has let go of the state lock.
    fn update_application_limited(&mut self, bwe: &super::BandwidthEstimator) -> bool {
        let Some(sent_bitrate) = self.sent_bitrate(now()) else {
            return false;
        };

        let target = self.estimated_bitrate as f64;
        let limited = if self.application_limited {
            sent_bitrate < ALR_EXIT_RATIO * target
        } else {
            sent_bitrate < ALR_ENTER_RATIO * target
        };

        if limited == self.application_limited {
            return false;
        }

        gst::info!(
            CAT,
            obj = bwe,
            "{} the application-limited region: sending {}ps of the {}ps estimated",
            if limited { "Entering" } else { "Leaving" },
            human_kbits(sent_bitrate),
            human_kbits(target),
        );

        self.application_limited = limited;
        if !limited {
            self.leave_application_limited();
        }

        true
    }

    /// Drops what the element learned while it had nothing to send.
    fn leave_application_limited(&mut self) {
        // The received packets in the window all come from the
        // application-limited period, so `effective_bitrate()` measures how
        // little content there was rather than what the link can carry. Left
        // in place it would anchor the first decrease taken after the exit
        // to that rate, which is exactly the collapse this is here to avoid.
        self.detector.last_received_packets.clear();

        // Same reasoning for the delay estimate: the delay variations
        // measured over sparse traffic say little about the link that is
        // about to carry real content, so widen the estimator's uncertainty
        // and let the fresh measurements move it.
        self.detector.estimator_impl.expect_fast_rate_change();
    }

    fn compute_increased_rate(&mut self, bwe: &super::BandwidthEstimator) -> Option<Bitrate> {
        let now = now();
        let target_bitrate = self.target_bitrate_on_delay as f64;
        let effective_bitrate = self.detector.effective_bitrate();
        let time_since_last_update_ms = match self.last_increase_on_delay {
            None => 0.,
            Some(prev) => {
                if now - prev < DELAY_UPDATE_INTERVAL {
                    return None;
                }

                Duration::try_from(now - prev).unwrap().whole_milliseconds() as f64
            }
        };

        if effective_bitrate as f64 - target_bitrate > 5. * target_bitrate / 100. {
            gst::info!(
                CAT,
                "Effective rate {} >> target bitrate {} - we should avoid that \
                 as much as possible fine tuning the encoder",
                human_kbits(effective_bitrate),
                human_kbits(target_bitrate)
            );
        }

        self.last_increase_on_delay = Some(now);

        // The link is carrying more than the capacity we remember, so that
        // memory describes a link we are not on any more: drop it and go
        // discover the new one. libwebrtc resets on the same condition, in
        // `AimdRateControl::ChangeBitrate`'s increase branch.
        if self.link_capacity.is_above(effective_bitrate) {
            gst::log!(
                CAT,
                obj = bwe,
                "Effective bitrate {}ps is above the link capacity band, forgetting it",
                human_kbits(effective_bitrate),
            );
            self.link_capacity.reset();
        }

        // Around the capacity we know about, creep up additively; anywhere
        // else, ramp multiplicatively to find the capacity. libwebrtc makes
        // this decision on whether it has a capacity estimate at all, having
        // just reset it above when the measurements left the band; asking
        // where the target sits in the band is the same decision, one step
        // earlier, and it also keeps the memory of a capacity we are still
        // climbing back up to after a decrease.
        if self.link_capacity.is_close(self.target_bitrate_on_delay) {
            let bits_per_frame = target_bitrate / 30.;
            let packets_per_frame = f64::ceil(bits_per_frame / (1200. * 8.));
            let avg_packet_size_bits = bits_per_frame / packets_per_frame;

            let rtt_ms = self.detector.rtt().whole_milliseconds() as f64;
            let response_time_ms = 100. + rtt_ms;
            let alpha = 0.5 * f64::min(time_since_last_update_ms / response_time_ms, 1.0);
            let threshold_on_effective_bitrate = 1.5 * effective_bitrate as f64;
            let increase = f64::max(
                1000.0f64,
                f64::min(
                    alpha * avg_packet_size_bits,
                    // Stuffing should ensure that the effective bitrate is not
                    // < target bitrate, still, make sure to always increase
                    // the bitrate by a minimum amount of 160.bits
                    f64::max(
                        threshold_on_effective_bitrate - self.target_bitrate_on_delay as f64,
                        160.0,
                    ),
                ),
            );

            /* Additive increase */
            self.last_control_op =
                BandwidthEstimationOp::Increase(format!("Additive ({})", human_kbits(increase)));
            Some((self.target_bitrate_on_delay as f64 + increase) as Bitrate)
        } else {
            let eta = 1.08_f64.powf(f64::min(time_since_last_update_ms / 1000., 1.0));
            let rate = eta * self.target_bitrate_on_delay as f64;

            assert!(
                rate >= self.target_bitrate_on_delay as f64,
                "Increase: {rate} - {eta}"
            );

            // Maximum increase to 1.5 * received rate
            let received_max = 1.5 * effective_bitrate as f64;

            if rate > received_max && received_max > self.target_bitrate_on_delay as f64 {
                gst::log!(
                    CAT,
                    obj = bwe,
                    "Increasing == received_max rate: {}ps - effective bitrate: {}ps",
                    human_kbits(received_max),
                    human_kbits(effective_bitrate),
                );

                self.last_control_op = BandwidthEstimationOp::Increase(format!(
                    "Using 1.5*effective_rate({})",
                    human_kbits(effective_bitrate)
                ));
                Some(received_max as Bitrate)
            } else if rate < self.target_bitrate_on_delay as f64 {
                gst::log!(
                    CAT,
                    obj = bwe,
                    "Rate < target, returning {}ps - effective bitrate: {}ps",
                    human_kbits(self.target_bitrate_on_delay),
                    human_kbits(effective_bitrate),
                );

                None
            } else {
                gst::log!(
                    CAT,
                    obj = bwe,
                    "Increase mult {eta}x{}ps={}ps - effective bitrate: {}ps",
                    human_kbits(self.target_bitrate_on_delay),
                    human_kbits(rate),
                    human_kbits(effective_bitrate),
                );

                self.last_control_op =
                    BandwidthEstimationOp::Increase(format!("Multiplicative x{eta}"));
                Some(rate as Bitrate)
            }
        }
    }

    fn set_bitrate(
        &mut self,
        bwe: &super::BandwidthEstimator,
        bitrate: Bitrate,
        controller_type: ControllerType,
    ) -> bool {
        let prev_bitrate = Bitrate::min(self.target_bitrate_on_delay, self.target_bitrate_on_loss);

        // Ensure min_bitrate <= max_bitrate to avoid panic in clamp()
        let (min_bitrate, max_bitrate) = if self.min_bitrate <= self.max_bitrate {
            (self.min_bitrate, self.max_bitrate)
        } else {
            gst::error!(
                CAT,
                obj = bwe,
                "min_bitrate ({}) > max_bitrate ({}), using max_bitrate for both to avoid panic",
                self.min_bitrate,
                self.max_bitrate
            );
            (self.max_bitrate, self.max_bitrate)
        };

        match controller_type {
            ControllerType::Loss => {
                self.target_bitrate_on_loss = bitrate.clamp(min_bitrate, max_bitrate)
            }

            ControllerType::Delay => {
                self.target_bitrate_on_delay = bitrate.clamp(min_bitrate, max_bitrate)
            }
        }

        let target_bitrate =
            Bitrate::min(self.target_bitrate_on_delay, self.target_bitrate_on_loss)
                .clamp(min_bitrate, max_bitrate);

        if target_bitrate == prev_bitrate {
            return false;
        }

        gst::info!(
            CAT,
            obj = bwe,
            "{controller_type:?}: {}ps => {}ps ({:?}) - effective bitrate: {}ps",
            human_kbits(prev_bitrate),
            human_kbits(target_bitrate),
            self.last_control_op,
            human_kbits(self.detector.effective_bitrate()),
        );

        self.estimated_bitrate = target_bitrate;

        true
    }

    fn loss_control(&mut self, bwe: &super::BandwidthEstimator) -> bool {
        let loss_ratio = self.detector.loss_ratio();
        let now = now();

        if loss_ratio > LOSS_DECREASE_THRESHOLD
            && (now - self.last_decrease_on_loss) > LOSS_UPDATE_INTERVAL
        {
            let factor = 1. - (0.5 * loss_ratio);

            self.last_control_op =
                BandwidthEstimationOp::Decrease(format!("High loss detected ({loss_ratio:2}"));
            self.last_decrease_on_loss = now;

            self.set_bitrate(
                bwe,
                (self.target_bitrate_on_loss as f64 * factor) as Bitrate,
                ControllerType::Loss,
            )
        } else if loss_ratio < LOSS_INCREASE_THRESHOLD
            && (now - self.last_increase_on_loss) > LOSS_UPDATE_INTERVAL
        {
            self.last_control_op = BandwidthEstimationOp::Increase("Low loss".into());
            self.last_increase_on_loss = now;

            self.set_bitrate(
                bwe,
                (self.target_bitrate_on_loss as f64 * LOSS_INCREASE_FACTOR) as Bitrate,
                ControllerType::Loss,
            )
        } else {
            false
        }
    }

    fn delay_control(&mut self, bwe: &super::BandwidthEstimator) -> bool {
        match self.detector.usage {
            NetworkUsage::Normal => match self.last_control_op {
                BandwidthEstimationOp::Increase(..) | BandwidthEstimationOp::Hold => {
                    if let Some(bitrate) = self.compute_increased_rate(bwe) {
                        return self.set_bitrate(bwe, bitrate, ControllerType::Delay);
                    }
                }
                _ => (),
            },
            NetworkUsage::Over => {
                let now = now();
                if self.application_limited {
                    // The decrease below would take the target down to a
                    // multiple of the effective bitrate, but while we are
                    // application limited that is the rate of the content we
                    // happen to have, not the rate the link would carry.
                    // Adopting it here is what turns a quiet moment into a
                    // collapsed estimate that then takes 1.08^t to climb back
                    // out of. Hold the target instead. Loss based decreases
                    // (`loss_control`) are untouched: packets that were sent
                    // and did not arrive say something about the link no
                    // matter how little we sent.
                    gst::debug!(
                        CAT,
                        obj = bwe,
                        "Over use detected while application limited, holding \
                         the target at {}ps: {:#?}",
                        human_kbits(self.estimated_bitrate),
                        self.detector,
                    );
                } else if now - self.last_decrease_on_delay > DELAY_UPDATE_INTERVAL {
                    let effective_bitrate = self.detector.effective_bitrate();
                    if effective_bitrate == 0 {
                        // Nothing has arrived since the received packet
                        // window was last emptied, so there is no measurement
                        // of what the link carries to decrease onto: the
                        // target below would be `0.85 x 0`, which is the
                        // minimum bitrate after clamping, and the same 0
                        // would go into `link_capacity`. That window is empty
                        // right after leaving the application-limited region
                        // if the feedback that took us out reported every
                        // packet lost, which is what a resume burst into a
                        // path that cannot take it looks like. Hold, and
                        // leave the loss controller to act on the loss.
                        gst::debug!(
                            CAT,
                            obj = bwe,
                            "Over use detected with no received packets to \
                             measure, holding the target at {}ps: {:#?}",
                            human_kbits(self.estimated_bitrate),
                            self.detector,
                        );
                    } else {
                        // Back off onto a fraction of what the link was
                        // measured to carry, and never higher than where we
                        // already are: a decrease that raises the target is
                        // not a decrease, and the measured rate can exceed
                        // the target after a burst that followed an
                        // application-limited period.
                        //
                        // The vendored element also took `0.95 *
                        // estimated_bitrate` into that min, which turned
                        // every overuse where the target sat far below the
                        // measured rate into a 5% micro step: a series of
                        // them ratchets the target down 5% at a time without
                        // ever deciding anything about the link.
                        let target = f64::min(
                            self.estimated_bitrate as f64,
                            BETA * effective_bitrate as f64,
                        );
                        self.last_control_op = BandwidthEstimationOp::Decrease(format!(
                            "Over use detected {:#?}",
                            self.detector
                        ));

                        // The link is carrying far less than the capacity we
                        // remember: that memory is stale, so drop it and let
                        // the sample below become the estimate rather than
                        // being averaged against a link we are not on any
                        // more. Same condition as libwebrtc's decrease
                        // branch, which resets "to allow an immediate update
                        // in OnOveruseDetected".
                        if self.link_capacity.is_below(effective_bitrate) {
                            self.link_capacity.reset();
                        }
                        // Only ever sampled here, which the branch above
                        // keeps out of the application-limited region.
                        self.link_capacity.update(effective_bitrate);
                        self.last_decrease_on_delay = now;

                        return self.set_bitrate(bwe, target as Bitrate, ControllerType::Delay);
                    }
                }
            }
            NetworkUsage::Under => {
                if let BandwidthEstimationOp::Increase(..) = self.last_control_op
                    && let Some(bitrate) = self.compute_increased_rate(bwe)
                {
                    return self.set_bitrate(bwe, bitrate, ControllerType::Delay);
                }
            }
        }

        self.last_control_op = BandwidthEstimationOp::Hold;

        false
    }
}

pub struct BandwidthEstimator {
    state: Mutex<State>,

    srcpad: gst::Pad,
    sinkpad: gst::Pad,
}

impl BandwidthEstimator {
    fn push_list(&self, list: BufferList) -> Result<gst::FlowSuccess, gst::FlowError> {
        let mut res = Ok(gst::FlowSuccess::Ok);
        for buf in list {
            res = self.srcpad.push(buf);
            if res.is_err() {
                break;
            }
        }

        self.state.lock().unwrap().flow_return = res;

        res
    }

    fn start_task(&self, bwe: &super::BandwidthEstimator) -> Result<(), glib::BoolError> {
        let weak_bwe = bwe.downgrade();
        let weak_pad = self.srcpad.downgrade();
        let clock = gst::SystemClock::obtain();

        bwe.imp().state.lock().unwrap().clock_entry =
            Some(clock.new_single_shot_id(clock.time() + dur2ts(BURST_TIME)));

        self.srcpad.start_task(move || {
            let pause = || {
                if let Some(pad) = weak_pad.upgrade() {
                    let _ = pad.pause_task();
                }
            };
            let bwe = weak_bwe
                .upgrade()
                .expect("bwe destroyed while its srcpad task is still running?");

            let lock_state = || bwe.imp().state.lock().unwrap();

            let clock_entry = match lock_state().clock_entry.take() {
                Some(id) => id,
                _ => {
                    gst::info!(CAT, "Pausing task as our clock entry is not set anymore");
                    return pause();
                }
            };

            if let (Err(err), _) = clock_entry.wait() {
                match err {
                    gst::ClockError::Early => (),
                    _ => {
                        gst::error!(CAT, "Got error {err:?} on the clock, pausing task");

                        lock_state().flow_return = Err(gst::FlowError::Flushing);

                        return pause();
                    }
                }
            }
            let list = {
                let mut state = lock_state();
                clock
                    .single_shot_id_reinit(&clock_entry, clock.time() + dur2ts(BURST_TIME))
                    .unwrap();
                state.clock_entry = Some(clock_entry);
                state.create_buffer_list(&bwe)
            };

            if !list.is_empty()
                && let Err(err) = bwe.imp().push_list(list)
            {
                if err != gst::FlowError::Flushing {
                    gst::error!(CAT, obj = bwe, "pause task, reason: {err:?}");
                }
                pause()
            }
        })?;

        Ok(())
    }

    fn src_activatemode(
        &self,
        _pad: &gst::Pad,
        bwe: &super::BandwidthEstimator,
        mode: gst::PadMode,
        active: bool,
    ) -> Result<(), gst::LoggableError> {
        if let gst::PadMode::Push = mode {
            if active {
                let mut state = self.state.lock().unwrap();
                state.flow_return = Ok(gst::FlowSuccess::Ok);
                // Nothing was sent before now, so measure the sent rate from
                // here rather than from whenever the element was built.
                state.sent_bytes.clear();
                state.sent_window_start = now();
                drop(state);

                self.start_task(bwe)?;
            } else {
                let mut state = self.state.lock().unwrap();
                state.flow_return = Err(gst::FlowError::Flushing);
                drop(state);

                self.srcpad.stop_task()?;
            }

            Ok(())
        } else {
            Err(gst::LoggableError::new(
                *CAT,
                glib::bool_error!("Unsupported pad mode {mode:?}"),
            ))
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for BandwidthEstimator {
    const NAME: &'static str = "GstClarityGCCBwE";
    type Type = super::BandwidthEstimator;
    type ParentType = gst::Element;

    fn with_class(klass: &Self::Class) -> Self {
        let templ = klass.pad_template("sink").unwrap();
        let sinkpad = gst::Pad::builder_from_template(&templ)
            .chain_function(|_pad, parent, buffer| {
                BandwidthEstimator::catch_panic_pad_function(
                    parent,
                    || Err(gst::FlowError::Error),
                    |this| {
                        let mut state = this.state.lock().unwrap();
                        state.buffers.push_front(buffer);

                        state.flow_return
                    },
                )
            })
            .flags(gst::PadFlags::PROXY_CAPS | gst::PadFlags::PROXY_ALLOCATION)
            .build();

        let templ = klass.pad_template("src").unwrap();
        let srcpad = gst::Pad::builder_from_template(&templ)
            .event_function(|pad, parent, event| {
                BandwidthEstimator::catch_panic_pad_function(
                    parent,
                    || false,
                    |this| {
                        let bwe = this.obj();

                        if let Some(structure) = event.structure()
                            && structure.name() == "RTPTWCCPackets" {
                                let varray = structure.get::<glib::ValueArray>("packets").unwrap();
                                let mut packets = varray
                                    .iter()
                                    .filter_map(|s| {
                                        Packet::from_structure(&s.get::<gst::Structure>().unwrap())
                                    })
                                    .collect::<Vec<Packet>>();

                                // The list of packets could be empty once parsed
                                if !packets.is_empty() {
                                    let mut logged_bitrates = None;

                                    let (bitrate_changed, application_limited_changed) = {
                                        let mut state = this.state.lock().unwrap();

                                        // Before folding this batch in: leaving
                                        // the application-limited region drops
                                        // the received packet window, and these
                                        // packets are the first ones that
                                        // describe the link again.
                                        let application_limited_changed =
                                            state.update_application_limited(&bwe);

                                        state.detector.update(&mut packets);
                                        let bitrate_updated_by_delay = state.delay_control(&bwe);
                                        let bitrate_updated_by_loss = state.loss_control(&bwe);
                                        let bitrate_changed = bitrate_updated_by_delay || bitrate_updated_by_loss;

                                        if bitrate_changed {
                                            // So we don't have to hold the state mutex while logging.
                                            logged_bitrates = Some((
                                                state.target_bitrate_on_delay,
                                                state.target_bitrate_on_loss,
                                            ));
                                        }

                                        (bitrate_changed, application_limited_changed)
                                    };

                                    if let Some(bitrates) = logged_bitrates {
                                        gst::log!(
                                            CAT,
                                            obj = bwe,
                                            "target bitrate on delay: {}ps - target bitrate on loss: {}ps",
                                            human_kbits(bitrates.0),
                                            human_kbits(bitrates.1),
                                        );
                                    }

                                    // The state lock is released by now, so
                                    // neither notify runs under it.
                                    if application_limited_changed {
                                        bwe.notify("application-limited")
                                    }

                                    if bitrate_changed {
                                        bwe.notify("estimated-bitrate")
                                    }
                                }
                            }

                        gst::Pad::event_default(pad, parent, event)
                    },
                )
            })
            .activatemode_function(|pad, parent, mode, active| {
                BandwidthEstimator::catch_panic_pad_function(
                    parent,
                    || {
                        Err(gst::loggable_error!(
                            CAT,
                            "Panic activating src pad with mode"
                        ))
                    },
                    |this| this.src_activatemode(pad, &this.obj(), mode, active),
                )
            })
            .flags(gst::PadFlags::PROXY_CAPS | gst::PadFlags::PROXY_ALLOCATION)
            .build();

        Self {
            state: Default::default(),
            srcpad,
            sinkpad,
        }
    }
}

impl ObjectImpl for BandwidthEstimator {
    fn constructed(&self) {
        self.parent_constructed();

        let obj = self.obj();
        obj.add_pad(&self.sinkpad).unwrap();
        obj.add_pad(&self.srcpad).unwrap();
    }

    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
            vec![
                /*
                 *  gcc:estimated-bitrate:
                 *
                 * Currently computed network bitrate, should be used
                 * to set encoders bitrate.
                 */
                glib::ParamSpecUInt::builder("estimated-bitrate")
                    .nick("Estimated Bitrate")
                    .blurb("Currently estimated bitrate. Can be set before starting
                     the element to configure the starting bitrate, in which case the
                     encoder should also use it as target bitrate. Can also be set
                     while playing to steer the estimate, e.g. from an out-of-band
                     signal; the value is clamped to [min-bitrate, max-bitrate].")
                    .minimum(1)
                    .maximum(u32::MAX)
                    .default_value(DEFAULT_MIN_BITRATE)
                    .mutable_playing()
                    .build(),
                glib::ParamSpecUInt::builder("min-bitrate")
                    .nick("Minimal Bitrate")
                    .blurb("Minimal bitrate to use (in bit/sec) when computing it through the bandwidth estimation algorithm")
                    .minimum(1)
                    .maximum(u32::MAX)
                    .default_value(DEFAULT_MIN_BITRATE)
                    .mutable_ready()
                    .build(),
                glib::ParamSpecUInt::builder("max-bitrate")
                    .nick("Maximum Bitrate")
                    .blurb("Maximum bitrate to use (in bit/sec) when computing it through the bandwidth estimation algorithm")
                    .minimum(1)
                    .maximum(u32::MAX)
                    .default_value(DEFAULT_MAX_BITRATE)
                    .mutable_ready()
                    .build(),
                glib::ParamSpecEnum::builder_with_default("estimator", Estimator::default())
                    .nick("Estimator")
                    .blurb("How to calculate the delay estimate that will be compared against the dynamic delay threshold.")
                    .mutable_ready()
                    .build(),
                /*
                 *  gcc:pacing-factor:
                 *
                 * The multiple of the estimated bitrate at which the internal
                 * pacer drains its buffer. libwebrtc's default pace multiplier
                 * is 2.5.
                 */
                glib::ParamSpecDouble::builder("pacing-factor")
                    .nick("Pacing Factor")
                    .blurb("Multiple of the estimated bitrate at which the internal pacer drains (libwebrtc's default pace multiplier is 2.5)")
                    .minimum(1.0)
                    .maximum(10.0)
                    .default_value(DEFAULT_PACING_FACTOR)
                    .mutable_playing()
                    .build(),
                /*
                 *  gcc:application-limited:
                 *
                 * Whether the element is currently sending less than its
                 * estimate allows, because the application has little to
                 * send. While that is the case the delay based controller
                 * holds its target instead of decreasing it, as the delay
                 * measurements describe the absent content rather than the
                 * link.
                 */
                glib::ParamSpecBoolean::builder("application-limited")
                    .nick("Application Limited")
                    .blurb("Whether the element is sending less than the estimated bitrate allows, in which case delay based decreases are held")
                    .default_value(false)
                    .read_only()
                    .build(),
            ]
        });

        PROPERTIES.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        match pspec.name() {
            "min-bitrate" => {
                let mut state = self.state.lock().unwrap();
                state.min_bitrate = value.get::<u32>().expect("type checked upstream");
            }
            "max-bitrate" => {
                let mut state = self.state.lock().unwrap();
                state.max_bitrate = value.get::<u32>().expect("type checked upstream");
            }
            "estimated-bitrate" => {
                let bitrate = value.get::<u32>().expect("type checked upstream");
                let mut state = self.state.lock().unwrap();

                // Below READY the element hasn't started its pad/streaming
                // thread yet, so there is no pacer or feedback loop whose
                // bookkeeping could go stale: keep the original unclamped
                // "set the starting bitrate" behaviour. From PAUSED onward
                // this is a live retarget, so clamp it and reset the
                // controllers' bookkeeping the same way a fresh element
                // would start out.
                if self.obj().current_state() <= gst::State::Ready {
                    state.target_bitrate_on_delay = bitrate;
                    state.target_bitrate_on_loss = bitrate;
                    state.estimated_bitrate = bitrate;
                } else {
                    let (min_bitrate, max_bitrate) = if state.min_bitrate <= state.max_bitrate {
                        (state.min_bitrate, state.max_bitrate)
                    } else {
                        (state.max_bitrate, state.max_bitrate)
                    };
                    let bitrate = bitrate.clamp(min_bitrate, max_bitrate);

                    state.target_bitrate_on_delay = bitrate;
                    state.target_bitrate_on_loss = bitrate;
                    // Read by the pacer's leaky bucket on its next call, so
                    // the drain rate reflects this write immediately.
                    state.estimated_bitrate = bitrate;

                    // This is an externally forced retarget, not the outcome
                    // of the delay/loss controllers reacting to feedback:
                    // reset their bookkeeping so the next feedback batch
                    // judges the new value on its own merits, rather than
                    // against timers left over from before the write.
                    // Otherwise the update-interval gates (keyed off
                    // `last_decrease_on_*`) could let a decrease fire
                    // immediately against stale state, and a large elapsed
                    // time since `last_increase_on_delay` could produce an
                    // oversized multiplicative jump on top of the write we
                    // just made.
                    state.last_increase_on_delay = None;
                    state.last_decrease_on_delay = now();
                    state.last_increase_on_loss = now();
                    state.last_decrease_on_loss = now();
                    // The write says nothing about the link, but the target it
                    // sets is the one the increase path now judges against the
                    // remembered band, and that band was measured around a
                    // target the application has just overridden. Start the
                    // capacity measurement over, the way a fresh element would.
                    state.link_capacity.reset();
                }

                // `notify` is emitted automatically once this call returns
                // (the property isn't EXPLICIT_NOTIFY), and `state` is
                // dropped before then, so no signal is emitted under the
                // lock.
            }
            "estimator" => {
                let mut state = self.state.lock().unwrap();
                state.estimator = value.get().unwrap();
                state.detector.estimator_impl = state.estimator.to_impl()
            }
            "pacing-factor" => {
                let mut state = self.state.lock().unwrap();
                state.pacing_factor = value.get::<f64>().expect("type checked upstream");
            }
            _ => unimplemented!(),
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "min-bitrate" => {
                let state = self.state.lock().unwrap();
                state.min_bitrate.to_value()
            }
            "max-bitrate" => {
                let state = self.state.lock().unwrap();
                state.max_bitrate.to_value()
            }
            "estimated-bitrate" => {
                let state = self.state.lock().unwrap();
                state.estimated_bitrate.to_value()
            }
            "estimator" => {
                let state = self.state.lock().unwrap();
                state.estimator.to_value()
            }
            "pacing-factor" => {
                let state = self.state.lock().unwrap();
                state.pacing_factor.to_value()
            }
            "application-limited" => {
                let state = self.state.lock().unwrap();
                state.application_limited.to_value()
            }
            _ => unimplemented!(),
        }
    }
}

impl GstObjectImpl for BandwidthEstimator {}

impl ElementImpl for BandwidthEstimator {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                "Google Congestion Control bandwidth estimator",
                "Network/WebRTC/RTP/Filter",
                "Estimates current network bandwidth using the Google Congestion Control algorithm \
                 notifying about it through the 'bitrate' property",
                "Thibault Saunier <tsaunier@igalia.com>",
            )
        });

        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            let caps = gst::Caps::builder_full()
                .structure(gst::Structure::builder("application/x-rtp").build())
                .build();

            let sinkpad_template = gst::PadTemplate::new(
                "sink",
                gst::PadDirection::Sink,
                gst::PadPresence::Always,
                &caps,
            )
            .unwrap();

            let srcpad_template = gst::PadTemplate::new(
                "src",
                gst::PadDirection::Src,
                gst::PadPresence::Always,
                &caps,
            )
            .unwrap();

            vec![sinkpad_template, srcpad_template]
        });

        PAD_TEMPLATES.as_ref()
    }
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests {
    use gstreamer as gst;

    use super::{Detector, Estimator, LinkCapacity, Packet};
    use time::Duration;

    /// One sample is enough to define a band that contains something. The
    /// plain variance of a single sample is 0, which makes `avg - 3*sigma ..
    /// avg + 3*sigma` the empty range and leaves nothing ever close to the
    /// capacity that was just measured; the normalized variance floor is what
    /// keeps the range usable.
    #[test]
    fn a_single_sample_defines_a_usable_band() {
        let mut capacity = LinkCapacity::default();
        assert_eq!(capacity.band(), None, "nothing measured yet");
        assert!(!capacity.is_close(2_000_000));

        capacity.update(2_000_000u32);
        let (low, high) = capacity.band().unwrap();
        assert!(
            low < 2_000_000 && 2_000_000 < high,
            "the sample should sit inside its own band, got {low}..{high}"
        );
        assert!(capacity.is_close(2_000_000));
        assert!(capacity.is_below(low - 1) && !capacity.is_close(low - 1));
        assert!(capacity.is_above(high + 1) && !capacity.is_close(high + 1));

        // Sampling the same capacity again keeps the band, rather than
        // narrowing it onto the average and losing the closeness test.
        capacity.update(2_000_000u32);
        assert_eq!(capacity.band(), Some((low, high)));

        capacity.reset();
        assert_eq!(capacity.band(), None, "a reset link capacity is unknown");
    }

    /// The average follows the samples, so a link that settles at a new
    /// capacity is remembered at that capacity.
    #[test]
    fn the_band_follows_the_samples() {
        let mut capacity = LinkCapacity::default();
        capacity.update(2_000_000u32);
        for _ in 0..10 {
            capacity.update(1_000_000u32);
        }

        let (low, high) = capacity.band().unwrap();
        assert!(
            capacity.is_close(1_000_000),
            "the band should have moved onto the new capacity, got {low}..{high}"
        );
        assert!(!capacity.is_close(2_000_000));
    }

    #[test]
    fn test_detector_ensure_no_leak() {
        gst::init().unwrap();
        let mut detector = Detector::new(Estimator::LinearRegression);
        for i in 0..100_i64 {
            let pkt = Packet {
                departure: Duration::ZERO,
                // Maximum i24 value.
                arrival: Duration::new((1 << 23) - 1 - (100 - i), 0),
                size: 0,
                seqnum: i as u64,
            };
            detector.update_last_received_packets(pkt);
        }

        for i in 0..100_i64 {
            let pkt = Packet {
                departure: Duration::ZERO,
                // Minimum i24 value.
                arrival: Duration::new(-(1 << 23) + i, 0),
                size: 0,
                seqnum: 100 + i as u64,
            };
            detector.update_last_received_packets(pkt);
        }
        // the actual number of packets should be 2, but it depends on the window size,
        // so just ensure it is lower than 10, as it will be 100 if it is failing.
        assert!(detector.last_received_packets.len() < 10);
    }
}
