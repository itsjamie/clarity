// SPDX-License-Identifier: MPL-2.0

//! Deterministic scenario rig for the bandwidth estimator.
//!
//! Drives a real element instance end-to-end: media buffers enter through the
//! sink pad, and synthetic "RTPTWCCPackets" feedback events are sent to the
//! src pad, modelling a bottleneck link with a fixed capacity, a base one-way
//! delay and optional periodic jitter pulses. When the offered rate exceeds
//! the link capacity the model's FIFO queue grows, spreading out arrival
//! timestamps exactly like a congested path would.
//!
//! Traffic is shaped like video: one burst of packets per frame interval.
//! That shape matters to the detector, whose packet grouping only completes
//! on inter-arrival gaps of at least `BURST_TIME`; a perfectly paced constant
//! rate stream never produces such gaps and leaves the delay estimator idle.
//!
//! Time never advances by sleeping: the estimator's clock reads are
//! redirected here (see `now()` in `imp.rs`) and the rig advances the shared
//! clock manually, one feedback interval at a time.

use gstreamer as gst;

use gst::{glib, prelude::*, subclass::prelude::*};
use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};
use time::Duration;

use super::{Bitrate, dur2ts};

pub(crate) mod clock {
    use std::sync::{LazyLock, Mutex};
    use std::time::Instant;
    use time::Duration;

    static BASE_INSTANT: LazyLock<Instant> = LazyLock::new(Instant::now);
    static NOW: Mutex<Duration> = Mutex::new(Duration::ZERO);

    /// The rig's clock as an `Instant`, consumed by the rate controllers.
    pub(crate) fn instant() -> Instant {
        *BASE_INSTANT + std::time::Duration::from_nanos(stream().whole_nanoseconds() as u64)
    }

    /// The rig's clock on the same timeline as the synthetic packet
    /// timestamps, consumed as the RTT reference in `Detector::update_rtts`.
    pub(crate) fn stream() -> Duration {
        *NOW.lock().unwrap()
    }

    pub(crate) fn advance_to(t: Duration) {
        let mut now = NOW.lock().unwrap();
        assert!(t >= *now, "the test clock only moves forward");
        *now = t;
    }
}

/// Scenario tests share the global test clock, so they run one at a time.
static RIG_LOCK: Mutex<()> = Mutex::new(());

/// How often the rig delivers a TWCC feedback event, mirroring the ~50ms
/// cadence of real transport-wide feedback.
const FEEDBACK_INTERVAL: Duration = Duration::milliseconds(50);

/// Periodic episodes of growing extra delay, as cross traffic or link-layer
/// contention would produce. During each pulse the arrival timestamps drift
/// later at `slope` seconds per second, then snap back when the pulse ends.
pub(crate) struct JitterPulse {
    /// Distance between the starts of two consecutive pulses.
    pub(crate) period: Duration,
    /// How long each pulse lasts.
    pub(crate) length: Duration,
    /// Extra delay accumulated per unit of time while a pulse is active,
    /// e.g. 0.2 adds 100ms of delay over a 500ms pulse.
    pub(crate) slope: f64,
}

pub(crate) struct LinkConfig {
    pub(crate) capacity_bps: u64,
    pub(crate) one_way_delay: Duration,
    pub(crate) jitter: Option<JitterPulse>,
}

pub(crate) struct TrafficConfig {
    /// Distance between two frame bursts, e.g. ~33ms for 30fps video.
    pub(crate) frame_interval: Duration,
    /// Size of each synthetic RTP packet, in bytes.
    pub(crate) packet_size: usize,
}

pub(crate) struct ScenarioConfig {
    pub(crate) link: LinkConfig,
    pub(crate) traffic: TrafficConfig,
    pub(crate) start_bitrate: Bitrate,
    pub(crate) min_bitrate: Bitrate,
    pub(crate) max_bitrate: Bitrate,
}

struct InFlight {
    seqnum: u64,
    departure: Duration,
    arrival: Duration,
    size: usize,
}

pub(crate) struct TestRig {
    bwe: crate::gcc::BandwidthEstimator,
    upstream: gst::Pad,
    #[allow(dead_code)]
    downstream: gst::Pad,
    link: LinkConfig,
    traffic: TrafficConfig,

    // Bottleneck FIFO model
    busy_until: Duration,
    last_arrival: Duration,
    max_queue_delay: Duration,

    // Synthetic schedule
    epoch: Duration,
    now: Duration,
    next_frame: Duration,
    seqnum: u64,
    in_flight: VecDeque<InFlight>,

    trajectory: Vec<(Duration, Bitrate)>,

    _lock: MutexGuard<'static, ()>,
}

impl TestRig {
    pub(crate) fn new(config: ScenarioConfig) -> Self {
        gst::init().unwrap();
        let lock = RIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let bwe = glib::Object::new::<crate::gcc::BandwidthEstimator>();
        bwe.set_property("estimated-bitrate", config.start_bitrate);
        bwe.set_property("min-bitrate", config.min_bitrate);
        bwe.set_property("max-bitrate", config.max_bitrate);

        let upstream = gst::Pad::builder(gst::PadDirection::Src).build();
        let downstream = gst::Pad::builder(gst::PadDirection::Sink)
            .chain_function(|_pad, _parent, _buffer| Ok(gst::FlowSuccess::Ok))
            .build();
        upstream.set_active(true).unwrap();
        downstream.set_active(true).unwrap();
        upstream
            .link(&bwe.static_pad("sink").unwrap())
            .expect("link upstream pad");
        bwe.static_pad("src")
            .unwrap()
            .link(&downstream)
            .expect("link downstream pad");

        bwe.set_state(gst::State::Playing).unwrap();

        upstream.push_event(gst::event::StreamStart::new("rig"));
        upstream.push_event(gst::event::Caps::new(
            &gst::Caps::builder("application/x-rtp").build(),
        ));
        upstream.push_event(gst::event::Segment::new(
            gst::FormattedSegment::<gst::ClockTime>::new().as_ref(),
        ));

        let epoch = clock::stream();
        TestRig {
            bwe,
            upstream,
            downstream,
            link: config.link,
            traffic: config.traffic,
            busy_until: epoch,
            last_arrival: epoch,
            max_queue_delay: Duration::ZERO,
            epoch,
            now: epoch,
            next_frame: epoch,
            seqnum: 0,
            in_flight: VecDeque::new(),
            trajectory: Vec::new(),
            _lock: lock,
        }
    }

    pub(crate) fn estimate(&self) -> Bitrate {
        self.bwe.property::<u32>("estimated-bitrate")
    }

    /// The (elapsed, estimate) samples recorded so far, one per feedback
    /// interval.
    pub(crate) fn trajectory(&self) -> &[(Duration, Bitrate)] {
        &self.trajectory
    }

    /// The largest queueing (plus jitter) delay the link model produced.
    pub(crate) fn max_queue_delay(&self) -> Duration {
        self.max_queue_delay
    }

    /// Runs the scenario for `duration` of synthetic time. `offered_rate` is
    /// called once per feedback interval with the elapsed time and the
    /// current estimate, and returns the media bitrate to offer (in bit/sec,
    /// 0 for silence), which allows both open-loop schedules and closed-loop
    /// "encoder tracks the estimate" behaviour.
    pub(crate) fn run_for(
        &mut self,
        duration: Duration,
        mut offered_rate: impl FnMut(Duration, Bitrate) -> u64,
    ) {
        let end = self.now + duration;
        while self.now < end {
            let tick_end = self.now + FEEDBACK_INTERVAL;
            let rate = offered_rate(self.now - self.epoch, self.estimate());
            self.generate_packets(rate, tick_end);
            self.push_media_buffer(rate);
            self.now = tick_end;
            clock::advance_to(self.now);
            self.drain_pacer();
            self.deliver_feedback();
            self.trajectory.push((self.now - self.epoch, self.estimate()));
        }
    }

    fn generate_packets(&mut self, rate: u64, until: Duration) {
        // Local pacing of the packets within one frame burst.
        const INTRA_FRAME_GAP: Duration = Duration::microseconds(100);

        if rate == 0 {
            self.next_frame = until;
            return;
        }

        let packet_bits = (self.traffic.packet_size * 8) as u64;
        let frame_bits =
            rate * self.traffic.frame_interval.whole_nanoseconds() as u64 / 1_000_000_000;
        let packets_per_frame = frame_bits.div_ceil(packet_bits).max(1);
        while self.next_frame < until {
            for i in 0..packets_per_frame {
                let departure = self.next_frame + INTRA_FRAME_GAP * (i as u32);
                let arrival = self.transmit(departure);
                self.in_flight.push_back(InFlight {
                    seqnum: self.seqnum,
                    departure,
                    arrival,
                    size: self.traffic.packet_size,
                });
                self.seqnum += 1;
            }
            self.next_frame += self.traffic.frame_interval;
        }
    }

    fn transmit(&mut self, departure: Duration) -> Duration {
        let bits = (self.traffic.packet_size * 8) as u64;
        let tx = Duration::nanoseconds((bits * 1_000_000_000 / self.link.capacity_bps) as i64);
        let start = departure.max(self.busy_until);
        self.busy_until = start + tx;

        let mut arrival = self.busy_until + self.link.one_way_delay;
        if let Some(jitter) = &self.link.jitter {
            let phase = Duration::nanoseconds(
                ((departure - self.epoch).whole_nanoseconds() % jitter.period.whole_nanoseconds())
                    as i64,
            );
            if phase < jitter.length {
                arrival += phase * jitter.slope;
            }
        }

        self.max_queue_delay = self
            .max_queue_delay
            .max(arrival - departure - tx - self.link.one_way_delay);

        // A queue never reorders: a delayed packet delays the ones behind it.
        let arrival = arrival.max(self.last_arrival);
        self.last_arrival = arrival;
        arrival
    }

    /// Runs the pacer's leaky bucket once per feedback interval, the way the
    /// element's own srcpad task does at `BURST_TIME` intervals. That task is
    /// running too, but it wakes on the system clock, so driving the drain
    /// from here is what keeps what the element believes it sent on the same
    /// timeline as the synthetic packets.
    fn drain_pacer(&mut self) {
        let imp = self.bwe.imp();
        let mut state = imp.state.lock().unwrap();
        let _ = state.create_buffer_list(&self.bwe);
    }

    fn push_media_buffer(&mut self, rate: u64) {
        let bytes = ((rate / 8) as usize)
            .saturating_mul(FEEDBACK_INTERVAL.whole_milliseconds() as usize)
            / 1000;
        let bytes = bytes.min(100_000);
        if bytes == 0 {
            return;
        }

        let mut buffer = gst::Buffer::with_size(bytes).unwrap();
        buffer.get_mut().unwrap().set_pts(dur2ts(self.now - self.epoch));
        let _ = self.upstream.push(buffer);
    }

    #[allow(unsafe_code)]
    fn deliver_feedback(&mut self) {
        let mut structures = Vec::new();
        while self
            .in_flight
            .front()
            .is_some_and(|p| p.arrival <= self.now)
        {
            let p = self.in_flight.pop_front().unwrap();
            let s = gst::Structure::builder("RTPTWCCPacket")
                .field("lost", false)
                .field("seqnum", (p.seqnum & 0xffff) as u32)
                .field("size", p.size as u32)
                .field("local-ts", dur2ts(p.departure))
                .field("remote-ts", dur2ts(p.arrival));
            structures.push(s.build());
        }

        if structures.is_empty() {
            return;
        }

        // SAFETY: `ValueArray` is not `Send`, which the structure setters
        // require, but the event is built and consumed on this thread only.
        let packets =
            unsafe { glib::ValueArray::new(structures.iter()).to_value().into_send_value() };
        let s = gst::Structure::builder("RTPTWCCPackets")
            .field("packets", packets)
            .build();
        self.bwe
            .static_pad("src")
            .unwrap()
            .send_event(gst::event::CustomUpstream::new(s));
    }
}

impl Drop for TestRig {
    fn drop(&mut self) {
        let _ = self.bwe.set_state(gst::State::Null);
    }
}

/// One-call driver for scenario tests: runs `offered_rate` against a link for
/// `duration` and returns the estimate trajectory, one sample per feedback
/// interval.
pub(crate) fn run_scenario(
    config: ScenarioConfig,
    duration: Duration,
    offered_rate: impl FnMut(Duration, Bitrate) -> u64,
) -> Vec<(Duration, Bitrate)> {
    let mut rig = TestRig::new(config);
    rig.run_for(duration, offered_rate);
    rig.trajectory().to_vec()
}

#[cfg(test)]
mod scenarios {
    use super::*;

    /// Offered rate stays below a 5Mbps link: the estimate converges upward
    /// from its 1Mbps start and does not collapse.
    #[test]
    fn steady_below_capacity_converges_upward() {
        let trajectory = run_scenario(
            ScenarioConfig {
                link: LinkConfig {
                    capacity_bps: 5_000_000,
                    one_way_delay: Duration::milliseconds(20),
                    jitter: None,
                },
                traffic: TrafficConfig {
                    frame_interval: Duration::milliseconds(33),
                    packet_size: 1200,
                },
                start_bitrate: 1_000_000,
                min_bitrate: 100_000,
                max_bitrate: 8_192_000,
            },
            Duration::seconds(20),
            // A well-behaved encoder: track the estimate, but never offer
            // more than 4Mbps, safely below the link capacity.
            |_, estimate| u64::from(estimate).min(4_000_000),
        );

        let last = trajectory.last().unwrap().1;
        assert!(
            last >= 3_000_000,
            "estimate should have converged upward from 1Mbps, got {last}"
        );
        let lowest = trajectory.iter().map(|(_, e)| *e).min().unwrap();
        assert!(
            lowest >= 900_000,
            "estimate should not collapse on a clean link, dropped to {lowest}"
        );
    }

    /// Offered rate above a 1.5Mbps link: the queue grows, and the estimate
    /// backs off below the offered rate.
    #[test]
    fn overuse_backs_off_below_offered_rate() {
        let mut rig = TestRig::new(congested_scenario());

        rig.run_for(Duration::seconds(4), |_, _| 2_500_000);

        assert!(
            rig.max_queue_delay() > Duration::milliseconds(100),
            "overload should have built a queue, got {}",
            rig.max_queue_delay()
        );
        let last = rig.trajectory().last().unwrap().1;
        assert!(
            last < 1_500_000,
            "estimate should back off below the 2.5Mbps offered rate, got {last}"
        );
    }

    /// A long application-limited period (tiny packets at ~200kbps on a
    /// 5Mbps link, with occasional jitter pulses) drags the estimate toward
    /// the floor: the first jitter-triggered overuse snaps it to 0.85x the
    /// effective (application-limited) rate and the capped increase path
    /// cannot climb back out. This characterizes the known-bad behaviour
    /// in-element ALR detection is about to fix; the assertion documents the
    /// baseline rather than blessing it.
    #[test]
    fn idle_application_limited_estimate_collapses() {
        let trajectory = run_scenario(idle_scenario(), Duration::seconds(15), |_, _| 200_000);

        let last = trajectory.last().unwrap().1;
        assert!(
            last < 500_000,
            "the unmodified element is expected to collapse the idle \
             estimate, got {last} from its 2048kbps start"
        );
    }

    /// A 1.5Mbps link with no jitter, offered more than it can carry: the
    /// queue grows and the delay along with it, real congestion.
    fn congested_scenario() -> ScenarioConfig {
        ScenarioConfig {
            link: LinkConfig {
                capacity_bps: 1_500_000,
                one_way_delay: Duration::milliseconds(20),
                jitter: None,
            },
            traffic: TrafficConfig {
                frame_interval: Duration::milliseconds(33),
                packet_size: 1200,
            },
            start_bitrate: 2_000_000,
            min_bitrate: 100_000,
            max_bitrate: 8_192_000,
        }
    }

    /// A 5Mbps link carrying ~200kbps of small packets, with jitter pulses
    /// that periodically look like overuse: the shape of a screen share of a
    /// still window.
    fn idle_scenario() -> ScenarioConfig {
        ScenarioConfig {
            link: LinkConfig {
                capacity_bps: 5_000_000,
                one_way_delay: Duration::milliseconds(20),
                jitter: Some(JitterPulse {
                    period: Duration::seconds(2),
                    length: Duration::milliseconds(500),
                    slope: 0.2,
                }),
            },
            traffic: TrafficConfig {
                frame_interval: Duration::milliseconds(33),
                packet_size: 250,
            },
            start_bitrate: 2_048_000,
            min_bitrate: 100_000,
            max_bitrate: 8_192_000,
        }
    }

}
