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
use std::sync::{Arc, Mutex, MutexGuard};
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
    report_lost: bool,

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
            report_lost: false,
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

    /// Changes what the bottleneck can carry from here on, the way a path
    /// that gains or loses capacity mid-session does. Packets already in
    /// flight keep the arrival times the old capacity gave them.
    pub(crate) fn set_capacity(&mut self, capacity_bps: u64) {
        self.link.capacity_bps = capacity_bps;
    }

    /// The band the element currently remembers the link's capacity in, or
    /// `None` while it has measured no capacity at all.
    pub(crate) fn link_capacity_band(&self) -> Option<(Bitrate, Bitrate)> {
        self.bwe.imp().state.lock().unwrap().link_capacity.band()
    }

    /// The largest queueing (plus jitter) delay the link model produced.
    pub(crate) fn max_queue_delay(&self) -> Duration {
        self.max_queue_delay
    }

    /// Whether the delay based detector is reporting overuse right now. It
    /// only moves when a packet arrives, so feedback that reports everything
    /// lost leaves it where it was.
    pub(crate) fn overusing(&self) -> bool {
        self.bwe.imp().state.lock().unwrap().detector.usage == super::NetworkUsage::Over
    }

    /// From now on the feedback reports every packet as lost, the way a path
    /// that drops everything it is handed would. Lost packets carry no
    /// arrival timestamp, so they never enter the detector's received packet
    /// window.
    pub(crate) fn report_all_lost(&mut self, lost: bool) {
        self.report_lost = lost;
    }

    /// Records every `notify::application-limited` from now on, as (elapsed,
    /// new value) pairs.
    pub(crate) fn watch_application_limited(&self) -> Arc<Mutex<Vec<(Duration, bool)>>> {
        let seen: Arc<Mutex<Vec<(Duration, bool)>>> = Default::default();
        let recorder = seen.clone();
        let epoch = self.epoch;
        self.bwe
            .connect_notify(Some("application-limited"), move |bwe, _| {
                recorder.lock().unwrap().push((
                    clock::stream() - epoch,
                    bwe.property::<bool>("application-limited"),
                ));
            });

        seen
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
                .field("lost", self.report_lost)
                .field("seqnum", (p.seqnum & 0xffff) as u32)
                .field("size", p.size as u32)
                .field("local-ts", dur2ts(p.departure));
            let s = if self.report_lost {
                s
            } else {
                s.field("remote-ts", dur2ts(p.arrival))
            };
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
    /// 5Mbps link, with occasional jitter pulses) never decreases the
    /// estimate. Baseline before the in-element ALR detection: this same
    /// scenario used to end below 500kbps, because the first
    /// jitter-triggered overuse snapped the estimate to 0.85x the effective
    /// (application-limited) rate and the capped increase path could not
    /// climb back out.
    #[test]
    fn idle_application_limited_estimate_holds() {
        let trajectory = run_scenario(idle_scenario(), Duration::seconds(15), |_, _| 200_000);

        let lowest = trajectory.iter().map(|(_, e)| *e).min().unwrap();
        assert!(
            lowest >= 2_048_000,
            "the estimate should never be decreased while application \
             limited, dropped to {lowest} from its 2048kbps start"
        );
    }

    /// Twenty seconds of near-idle traffic and then real content: within a
    /// few feedback batches of leaving the application-limited region the
    /// estimate has settled onto what the link carries, and it stays there.
    ///
    /// The estimate is above the link when content resumes, so it settles
    /// through a delay based decrease, and that decrease is the one the
    /// received packet window is reset for. Measured over the idle period
    /// the window reports the ~150kbps of content there was rather than the
    /// 1.5Mbps the link carries, which would snap the estimate an order of
    /// magnitude below the link. Climbing back out of that at 1.08^t takes
    /// some 30 seconds, 600 feedback batches.
    #[test]
    fn idle_then_busy_recovers_within_a_few_batches() {
        let mut rig = TestRig::new(narrow_link_scenario());

        rig.run_for(Duration::seconds(20), |_, _| 150_000);
        let idle_end = rig.estimate();
        assert!(
            idle_end >= 2_000_000,
            "the idle period should not have decreased the estimate, got {idle_end}"
        );

        // Content resumes, with the encoder targeting the estimate the way
        // `rate.rs` does.
        let busy_start = rig.trajectory().len();
        rig.run_for(Duration::seconds(3), |_, estimate| u64::from(estimate));

        // The link carries 1.5Mbps, and the bar is 80% of it.
        const CAPACITY: Bitrate = 1_500_000;
        const BATCHES: usize = 10;
        let busy = &rig.trajectory()[busy_start..];
        let on_the_link = |estimate: Bitrate| estimate >= CAPACITY / 5 * 4;

        assert!(
            busy.iter()
                .take(BATCHES)
                .any(|(_, estimate)| *estimate < idle_end && on_the_link(*estimate)),
            "within {BATCHES} feedback batches of content resuming the \
             estimate should have come off its idle value and onto the link, \
             got {:?}",
            busy.iter()
                .take(BATCHES)
                .map(|(_, e)| *e)
                .collect::<Vec<_>>()
        );
        for (i, (elapsed, estimate)) in busy.iter().take(BATCHES).enumerate() {
            // Anchored on the received packet window as it stood during the
            // idle period, this decrease would have gone to 0.85 x ~150kbps
            // instead, and climbing back out of that at 1.08^t takes some 30
            // seconds, 600 feedback batches.
            assert!(
                on_the_link(*estimate),
                "estimate fell to {estimate} at {elapsed}, {i} feedback \
                 batches after content resumed"
            );
        }

        // Past that the jitter pulses keep provoking decreases and the
        // estimate sawtooths below the link, which is the element's ordinary
        // response to a path that keeps adding delay, unchanged here.
    }

    /// Content resuming into a path that drops all of it: the feedback batch
    /// that takes the element out of the application-limited region reports
    /// every packet lost, so it puts nothing back into the received packet
    /// window the exit just cleared, and the delay based decrease has no
    /// measurement of the link to decrease onto. It holds, and the loss
    /// controller does the backing off. Without the empty-window guard the
    /// decrease adopted `0.85 x 0` and the estimate went from 2Mbps to the
    /// 100kbps floor in that one batch, with only 1.08^t to climb back out.
    #[test]
    fn all_lost_batch_on_exit_does_not_collapse_the_estimate() {
        let mut rig = TestRig::new(idle_scenario());

        // Sit application limited until one of the scenario's jitter pulses
        // has left the detector reporting overuse. That is the state the ALR
        // guard suppresses the decrease for, and nothing moves it back while
        // no packet arrives.
        let mut idle = Duration::ZERO;
        while idle < Duration::seconds(30)
            && !(rig.bwe.property::<bool>("application-limited") && rig.overusing())
        {
            rig.run_for(FEEDBACK_INTERVAL, |_, _| 200_000);
            idle += FEEDBACK_INTERVAL;
        }
        assert!(
            rig.bwe.property::<bool>("application-limited") && rig.overusing(),
            "expected an application-limited overuse to hold on the idle link"
        );
        let idle_end = rig.estimate();

        // Content resumes at the estimate and none of it arrives, up to the
        // batch that takes the element out of the application-limited region.
        // That is the batch the collapse happened on: it clears the received
        // packet window on the way out and the lost packets put nothing back.
        rig.report_all_lost(true);
        let mut burst = Duration::ZERO;
        while burst < Duration::seconds(2) && rig.bwe.property::<bool>("application-limited") {
            rig.run_for(FEEDBACK_INTERVAL, |_, estimate| u64::from(estimate));
            burst += FEEDBACK_INTERVAL;
        }
        assert!(
            !rig.bwe.property::<bool>("application-limited"),
            "sending at the estimate should have left the application-limited \
             region even with the feedback reporting it all lost"
        );
        assert!(
            rig.overusing(),
            "the detector should still be reporting the overuse it was in \
             when content resumed: nothing arrived to move it"
        );

        // The loss controller halves the target at most every 200ms, so it
        // alone cannot have taken the estimate below an eighth of where the
        // idle period left it over this burst. The collapse being guarded
        // against is the 100kbps floor, in one batch.
        let after_exit = rig.estimate();
        assert!(
            after_exit >= idle_end / 8,
            "an all-lost resume burst should back the estimate off through \
             the loss controller, not collapse it: {after_exit} from \
             {idle_end} over {burst}"
        );

        // And once packets arrive again it climbs back, which a delay based
        // target sitting on the floor could not do: from there only the
        // 1.08^t increase applies, some 30 seconds of it.
        rig.report_all_lost(false);
        rig.run_for(Duration::seconds(2), |_, estimate| u64::from(estimate));
        let recovered = rig.estimate();
        assert!(
            recovered >= 400_000,
            "the estimate should climb again once the path stops dropping \
             everything, got {recovered}"
        );
    }

    /// The overuse the `overuse_backs_off_below_offered_rate` scenario
    /// creates is genuine congestion, not an application-limited sender: the
    /// element never reports itself application limited there, so the
    /// decreases that scenario asserts are taken exactly as before.
    #[test]
    fn congestion_while_busy_is_not_application_limited() {
        let mut rig = TestRig::new(congested_scenario());
        let transitions = rig.watch_application_limited();

        rig.run_for(Duration::seconds(4), |_, _| 2_500_000);

        assert_eq!(
            *transitions.lock().unwrap(),
            vec![],
            "a sender offering more than the link carries is not application \
             limited at any point"
        );
        let last = rig.trajectory().last().unwrap().1;
        assert!(
            last < 1_500_000,
            "estimate should back off below the 2.5Mbps offered rate, got {last}"
        );
    }

    /// `application-limited` follows the traffic: it goes up once the sender
    /// has been offering ~200kbps against a multi-megabit estimate, and back
    /// down once content resumes, without flapping in between.
    #[test]
    fn application_limited_property_follows_the_schedule() {
        let mut rig = TestRig::new(idle_scenario());
        let transitions = rig.watch_application_limited();

        assert!(
            !rig.bwe.property::<bool>("application-limited"),
            "a sender that has not sent anything yet is not application limited"
        );

        rig.run_for(Duration::seconds(5), |_, _| 200_000);
        assert!(
            rig.bwe.property::<bool>("application-limited"),
            "200kbps against a 2Mbps estimate is application limited"
        );

        rig.run_for(Duration::seconds(2), |_, estimate| u64::from(estimate));
        assert!(
            !rig.bwe.property::<bool>("application-limited"),
            "a sender tracking the estimate is not application limited"
        );

        let transitions = transitions.lock().unwrap();
        let values = transitions.iter().map(|(_, v)| *v).collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![true, false],
            "expected one transition each way, got {transitions:?}"
        );
        assert!(
            transitions[0].0 <= Duration::milliseconds(500),
            "entering should take about the length of the sent-rate window, \
             took {}",
            transitions[0].0
        );
        assert!(
            transitions[1].0 - Duration::seconds(5) <= Duration::milliseconds(500),
            "leaving should take about the length of the sent-rate window, \
             took {}",
            transitions[1].0 - Duration::seconds(5)
        );
    }

    /// A clean link of a given capacity, carrying MTU-sized packets in
    /// per-frame bursts: the shape a real screen share has once there is
    /// something to send. Used by the AIMD scenarios, which change the
    /// capacity under a running element.
    fn stepping_scenario(capacity_bps: u64) -> ScenarioConfig {
        ScenarioConfig {
            link: LinkConfig {
                capacity_bps,
                one_way_delay: Duration::milliseconds(20),
                jitter: None,
            },
            traffic: TrafficConfig {
                frame_interval: Duration::milliseconds(33),
                packet_size: 1500,
            },
            start_bitrate: 3_000_000,
            min_bitrate: 100_000,
            max_bitrate: 5_000_000,
        }
    }

    /// What `rate.rs` asks of the element, offer exactly what was estimated,
    /// under a 4Mbps encoder ceiling. The ceiling keeps the sender off a
    /// standing queue on the 5Mbps side of the steps: at that capacity a
    /// 1500 byte packet serializes in 2.4ms, and once the queue stands the
    /// packets arrive that close together, which the pre-filter reads as one
    /// group that never ends (see the module docs). Below the capacity the
    /// queue drains between frames and the groups close on the frame gaps.
    fn busy(_: Duration, estimate: Bitrate) -> u64 {
        u64::from(estimate).min(4_000_000)
    }

    /// The window the estimate is expected to settle in on a 2Mbps link:
    /// `BETA` of what the link carries, up to the link itself.
    const ON_A_2MBPS_LINK: std::ops::RangeInclusive<Bitrate> = 1_600_000..=2_000_000;

    /// The link loses more than half of its capacity under a busy sender.
    /// The estimate settles onto what is left of it within a few seconds and
    /// stays there, rather than stepping its way down 5% at a time: the
    /// decrease adopts `BETA` x the rate the link is measured to carry, and
    /// the ones that follow cannot take it any lower while the link keeps
    /// carrying that much.
    #[test]
    fn capacity_step_down_settles_onto_the_new_link() {
        let mut rig = TestRig::new(stepping_scenario(5_000_000));

        rig.run_for(Duration::seconds(6), busy);
        let before_step = rig.estimate();
        assert!(
            before_step >= 4_000_000,
            "the estimate should have climbed onto the 5Mbps link first, got {before_step}"
        );

        rig.set_capacity(2_000_000);
        let step = rig.trajectory().len();
        rig.run_for(Duration::seconds(12), busy);

        // Three seconds of feedback batches to settle in, and the rest of the
        // run to show it stays.
        const SETTLING_BATCHES: usize = 60;
        let (settling, settled) = rig.trajectory()[step..].split_at(SETTLING_BATCHES);

        assert!(
            settling
                .iter()
                .any(|(_, estimate)| ON_A_2MBPS_LINK.contains(estimate)),
            "the estimate should have come down onto the 2Mbps link within \
             {SETTLING_BATCHES} feedback batches, got {:?}",
            settling.iter().map(|(_, e)| *e).collect::<Vec<_>>()
        );
        for (elapsed, estimate) in settled {
            assert!(
                ON_A_2MBPS_LINK.contains(estimate),
                "the estimate left the 2Mbps link at {elapsed}: {estimate}"
            );
        }
    }

    /// The link gets its capacity back. The estimate creeps additively while
    /// it is inside the band the element remembers the old capacity in, ramps
    /// multiplicatively once it is out of it, and reaches the new capacity
    /// well inside 30s without oscillating back down.
    #[test]
    fn capacity_step_up_recovers_through_the_remembered_band() {
        let mut rig = TestRig::new(stepping_scenario(2_000_000));

        rig.run_for(Duration::seconds(8), busy);
        let (low, high) = rig
            .link_capacity_band()
            .expect("decreases on the 2Mbps link should have measured its capacity");
        assert!(
            (1_800_000..=2_200_000).contains(&low) && (1_800_000..=2_200_000).contains(&high),
            "the remembered band should sit around the 2Mbps the link \
             carried, got {low}..{high}"
        );

        rig.set_capacity(5_000_000);
        let step = rig.trajectory().len();
        rig.run_for(Duration::seconds(30), busy);
        let recovery = &rig.trajectory()[step..];

        let width = high - low;
        let step_at = recovery[0].0;
        let time_at = |target: Bitrate| {
            recovery
                .iter()
                .find(|(_, estimate)| *estimate >= target)
                .map(|(elapsed, _)| *elapsed - step_at)
        };
        let in_band = FEEDBACK_INTERVAL
            * recovery
                .iter()
                .filter(|(_, estimate)| (low..high).contains(estimate))
                .count() as u32;
        // The first `width` of estimate above the band is the one the
        // capacity memory is dropped during, so measure the multiplicative
        // ramp over the one after it.
        let above_band = time_at(high + 2 * width).expect("recovery should pass the band")
            - time_at(high + width).expect("recovery should pass the band");

        assert!(
            in_band > Duration::seconds(2),
            "the estimate should creep additively while it is inside the \
             remembered band, crossed its {width}bps in {in_band}"
        );
        assert!(
            above_band * 2 < in_band,
            "the estimate should ramp multiplicatively once past the \
             remembered band: {width}bps took {above_band} there against \
             {in_band} inside the band"
        );

        let recovered =
            time_at(4_000_000).expect("the estimate should recover onto the 5Mbps link");
        assert!(
            recovered < Duration::seconds(25),
            "recovery onto the 5Mbps link took {recovered}"
        );
        let after_recovery = recovery
            .iter()
            .skip_while(|(elapsed, _)| *elapsed - step_at < recovered)
            .map(|(_, estimate)| *estimate)
            .min()
            .unwrap();
        assert!(
            after_recovery >= 3_000_000,
            "the estimate should stay on the link it recovered onto, fell \
             back to {after_recovery}"
        );
    }

    /// The increase that follows a decrease does not undo it in a batch or
    /// two: the target comes back up towards the capacity the element
    /// remembers, not straight past it.
    #[test]
    fn the_increase_after_a_decrease_does_not_overshoot() {
        let mut rig = TestRig::new(stepping_scenario(5_000_000));

        // Settle onto a 2Mbps link, so that the decreases from here on are
        // the ordinary ones taken against a capacity the element knows.
        rig.run_for(Duration::seconds(6), busy);
        rig.set_capacity(2_000_000);
        rig.run_for(Duration::seconds(6), busy);

        let settled = rig.trajectory().len();
        rig.run_for(Duration::seconds(6), busy);

        // The first decrease of the settled sawtooth, and the second worth of
        // feedback batches that follows it.
        let trajectory = &rig.trajectory()[settled..];
        let decrease = trajectory
            .windows(2)
            .position(|w| w[1].1 < w[0].1)
            .expect("the settled sawtooth should contain a decrease");
        let before = trajectory[decrease].1;
        let after = &trajectory[decrease + 1..];
        assert!(
            after[0].1 < before,
            "expected a decrease from {before}, got {}",
            after[0].1
        );

        const BATCHES: usize = 20;
        for (elapsed, estimate) in after.iter().take(BATCHES) {
            assert!(
                *estimate <= before,
                "the estimate was back above the {before} it was decreased \
                 from at {elapsed}: {estimate}"
            );
        }
    }

    /// Nothing measured while the sender is application limited goes into the
    /// link capacity: the rate it sends at then is the rate of the content it
    /// happens to have, and remembering it as the link's capacity would leave
    /// the increase creeping additively towards a rate the link never
    /// imposed. The jitter pulses of this scenario do produce overuses, they
    /// are just held rather than decreased on (see
    /// `idle_application_limited_estimate_holds`).
    #[test]
    fn application_limited_traffic_never_measures_the_link() {
        let mut rig = TestRig::new(idle_scenario());

        rig.run_for(Duration::seconds(15), |_, _| 200_000);

        assert!(
            rig.bwe.property::<bool>("application-limited"),
            "200kbps against a 2Mbps estimate is application limited"
        );
        assert_eq!(
            rig.link_capacity_band(),
            None,
            "the link capacity should be unknown after a run that only ever \
             sent ~200kbps of the ~5Mbps the link carries"
        );
    }

    /// A decrease never raises the target. The loss controller can have the
    /// estimate pinned well below what the link is measured to be carrying,
    /// and `BETA` x that measurement is then above the target the delay
    /// controller is decreasing from: adopting it would turn an overuse into
    /// an increase.
    #[test]
    fn an_overuse_decrease_never_raises_the_target() {
        let _lock = RIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        gst::init().unwrap();

        let bwe = glib::Object::new::<crate::gcc::BandwidthEstimator>();
        bwe.set_property("min-bitrate", 100_000u32);
        bwe.set_property("max-bitrate", 8_192_000u32);

        // Past the delay controller's update interval, so the decrease below
        // is not gated on it.
        let start = clock::stream();
        clock::advance_to(start + Duration::seconds(1));

        let imp = bwe.imp();
        let mut state = imp.state.lock().unwrap();

        // 20 packets of 2500 bytes over 190ms of arrivals, ~2.1Mbps of
        // measured link.
        for i in 0..20i64 {
            let departure = start + Duration::milliseconds(10 * i);
            state
                .detector
                .update_last_received_packets(super::super::Packet {
                    departure,
                    arrival: departure + Duration::milliseconds(20),
                    size: 2_500,
                    seqnum: i as u64,
                });
        }
        let effective = state.detector.effective_bitrate();
        assert!(
            effective > 2_000_000,
            "the window should measure the ~2.1Mbps it was fed, got {effective}"
        );

        // The loss controller has the estimate down at 500kbps while the
        // delay controller's own target is still where it was.
        state.estimated_bitrate = 500_000;
        state.target_bitrate_on_loss = 500_000;
        state.target_bitrate_on_delay = 3_000_000;
        state.detector.usage = super::super::NetworkUsage::Over;

        assert!(
            !state.delay_control(&bwe),
            "a decrease onto a rate above the estimate should leave it alone"
        );
        assert!(
            state.target_bitrate_on_delay <= 500_000,
            "the decrease raised the delay target to {} from the 500kbps \
             estimate, on a link measured at {effective}",
            state.target_bitrate_on_delay
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

    /// The same shape of link, narrow enough that ordinary 1200 byte packets
    /// take longer than `BURST_TIME` to serialize onto it. That keeps the
    /// detector's packet grouping working once the link saturates: on a fast
    /// link a saturating sender's packets arrive back to back, which the
    /// pre-filter reads as one group that never ends (see the module docs).
    fn narrow_link_scenario() -> ScenarioConfig {
        ScenarioConfig {
            link: LinkConfig {
                capacity_bps: 1_500_000,
                one_way_delay: Duration::milliseconds(20),
                jitter: Some(JitterPulse {
                    period: Duration::seconds(2),
                    length: Duration::milliseconds(500),
                    slope: 0.2,
                }),
            },
            traffic: TrafficConfig {
                frame_interval: Duration::milliseconds(33),
                packet_size: 1200,
            },
            start_bitrate: 2_000_000,
            min_bitrate: 100_000,
            // A ceiling the application would actually use, the way
            // `wire_gcc_bwe` derives it from the configured one.
            max_bitrate: 2_000_000,
        }
    }

    /// Writing `estimated-bitrate` while playing takes effect immediately
    /// and in range: the property getter, which is also what the pacer's
    /// leaky bucket reads for its drain rate, reflects the new value before
    /// any further feedback is processed.
    #[test]
    fn live_write_applies_immediately_in_range() {
        let mut rig = TestRig::new(ScenarioConfig {
            link: LinkConfig {
                capacity_bps: 10_000_000,
                one_way_delay: Duration::milliseconds(20),
                jitter: None,
            },
            traffic: TrafficConfig {
                frame_interval: Duration::milliseconds(33),
                packet_size: 1200,
            },
            start_bitrate: 1_000_000,
            min_bitrate: 200_000,
            max_bitrate: 4_000_000,
        });

        // Not sampled at t=0: let a bit of real traffic go by first.
        rig.run_for(Duration::milliseconds(500), |_, estimate| {
            u64::from(estimate).min(3_000_000)
        });

        rig.bwe.set_property("estimated-bitrate", 3_500_000u32);
        assert_eq!(
            rig.estimate(),
            3_500_000,
            "an in-range live write should apply exactly and immediately"
        );
    }

    /// A live write outside [min-bitrate, max-bitrate] clamps, both above
    /// and below.
    #[test]
    fn live_write_clamps_to_min_and_max_bitrate() {
        let rig = TestRig::new(ScenarioConfig {
            link: LinkConfig {
                capacity_bps: 10_000_000,
                one_way_delay: Duration::milliseconds(20),
                jitter: None,
            },
            traffic: TrafficConfig {
                frame_interval: Duration::milliseconds(33),
                packet_size: 1200,
            },
            start_bitrate: 1_000_000,
            min_bitrate: 200_000,
            max_bitrate: 4_000_000,
        });

        rig.bwe.set_property("estimated-bitrate", 9_000_000u32);
        assert_eq!(rig.estimate(), 4_000_000, "write above max-bitrate should clamp down to it");

        rig.bwe.set_property("estimated-bitrate", 50_000u32);
        assert_eq!(rig.estimate(), 200_000, "write below min-bitrate should clamp up to it");
    }

    /// `notify::estimated-bitrate` fires exactly once for a live write, and
    /// carries the clamped value rather than the raw one that was set.
    #[test]
    fn live_write_notifies_with_clamped_value() {
        let rig = TestRig::new(ScenarioConfig {
            link: LinkConfig {
                capacity_bps: 10_000_000,
                one_way_delay: Duration::milliseconds(20),
                jitter: None,
            },
            traffic: TrafficConfig {
                frame_interval: Duration::milliseconds(33),
                packet_size: 1200,
            },
            start_bitrate: 1_000_000,
            min_bitrate: 200_000,
            max_bitrate: 4_000_000,
        });

        let seen: std::sync::Arc<Mutex<Vec<u32>>> = Default::default();
        let seen_clone = seen.clone();
        rig.bwe.connect_notify(Some("estimated-bitrate"), move |obj, _| {
            seen_clone.lock().unwrap().push(obj.property::<u32>("estimated-bitrate"));
        });

        rig.bwe.set_property("estimated-bitrate", 9_000_000u32);

        assert_eq!(
            *seen.lock().unwrap(),
            vec![4_000_000],
            "notify should fire once, carrying the clamped value"
        );
    }

    /// A write before the element starts (state <= READY) keeps the
    /// original behaviour: the value is stored as-is, without clamping to
    /// [min-bitrate, max-bitrate]. This is `estimated-bitrate`'s documented
    /// use for configuring the starting bitrate.
    #[test]
    fn pre_start_write_is_unclamped_like_before() {
        gst::init().unwrap();
        let bwe = glib::Object::new::<crate::gcc::BandwidthEstimator>();
        bwe.set_property("min-bitrate", 200_000u32);
        bwe.set_property("max-bitrate", 4_000_000u32);

        assert!(bwe.current_state() <= gst::State::Ready);

        bwe.set_property("estimated-bitrate", 9_000_000u32);
        assert_eq!(
            bwe.property::<u32>("estimated-bitrate"),
            9_000_000,
            "pre-start writes should not be clamped, same as before this change"
        );
    }

    /// A live write that lands after a silent period (no in-flight packets,
    /// so no feedback events, so the delay controller's bookkeeping
    /// timestamps go stale while synthetic time keeps advancing under them)
    /// must not compound with the controller's own multiplicative increase
    /// on the next feedback batch. Without resetting `last_increase_on_delay`
    /// at write time, the stale elapsed-time computation applies an extra
    /// ~8% bump on top of the value that was just written, and the
    /// resulting overshoot goes on to trigger a real overuse decrease.
    #[test]
    fn live_write_after_silence_does_not_double_increase() {
        let mut rig = TestRig::new(ScenarioConfig {
            link: LinkConfig {
                capacity_bps: 10_000_000,
                one_way_delay: Duration::milliseconds(20),
                jitter: None,
            },
            traffic: TrafficConfig {
                frame_interval: Duration::milliseconds(33),
                packet_size: 1200,
            },
            start_bitrate: 500_000,
            min_bitrate: 100_000,
            max_bitrate: 8_192_000,
        });

        // Converge steadily, well below link capacity, no overuse: this is
        // what leaves `last_control_op` in a state (`Increase`/`Hold`) that
        // will make the delay controller call into the increase path again
        // as soon as feedback resumes.
        rig.run_for(Duration::seconds(3), |_, estimate| {
            u64::from(estimate).min(600_000)
        });

        // Silence long enough that, without a reset, elapsed time since the
        // delay controller's last update saturates the multiplicative
        // increase's `eta` term (capped at 1s).
        rig.run_for(Duration::milliseconds(1500), |_, _| 0);

        rig.bwe.set_property("estimated-bitrate", 4_000_000u32);
        assert_eq!(rig.estimate(), 4_000_000);

        // Resume traffic tracking the new estimate.
        let before_resume = rig.trajectory().len();
        rig.run_for(Duration::milliseconds(200), |_, estimate| u64::from(estimate));

        for (elapsed, estimate) in &rig.trajectory()[before_resume..] {
            assert_eq!(
                *estimate,
                4_000_000,
                "estimate drifted from the live write at {elapsed}: stale \
                 controller bookkeeping re-applied a multiplicative increase \
                 on top of it"
            );
        }
    }

    /// `pacing-factor` scales both the pacer's drain budget and its 30ms
    /// force-leak cap (see `State::create_buffer_list`). Feeds a constant
    /// burst offered at 1.5x a fixed 1Mbps estimate directly at the pacer,
    /// one `BURST_TIME` tick at a time (mirroring the cadence of the real
    /// background task, but driven synchronously so the test stays
    /// deterministic), and counts how many ticks had to force-leak to keep
    /// the queue under the cap.
    ///
    /// At the default factor of 1.0 the pacer's drain rate (1x the
    /// estimate) is below the 1.5x burst, so the queue outgrows the 30ms cap
    /// and force-leak has to engage repeatedly: this is the characterized
    /// baseline behaviour, unchanged by adding the property. At 2.5 (the
    /// value `wire_gcc_bwe` sets in `broadcast.rs`) the drain rate
    /// comfortably exceeds the 1.5x burst, so ordinary budget alone drains
    /// it and force-leak never triggers.
    #[test]
    fn pacing_factor_changes_force_leak_under_burst() {
        let (leaks_at_1_0, drained_at_1_0, offered) = run_pacer_burst(1.0);
        let (leaks_at_2_5, drained_at_2_5, offered_again) = run_pacer_burst(2.5);
        assert_eq!(offered, offered_again, "both runs offer the same burst");

        assert!(
            leaks_at_1_0 > 0,
            "factor 1.0: a 1.5x burst should force-leak, as the \
             characterization baseline shows"
        );
        assert_eq!(
            leaks_at_2_5, 0,
            "factor 2.5: a 1.5x burst is within the pacer's drain rate and \
             should never force-leak"
        );

        // Both factors are expected to actually drain the burst (force-leak
        // guarantees that at 1.0; ordinary budget does at 2.5) rather than
        // one of them silently stalling.
        assert!(
            drained_at_1_0 as f64 >= offered as f64 * 0.9,
            "factor 1.0 should still drain nearly all of the offered burst \
             via force-leak, drained {drained_at_1_0} of {offered} bits"
        );
        assert!(
            drained_at_2_5 as f64 >= offered as f64 * 0.9,
            "factor 2.5 should drain nearly all of the offered burst via \
             ordinary budget, drained {drained_at_2_5} of {offered} bits"
        );
    }

    /// Drives `State::create_buffer_list` directly (bypassing the real
    /// background pacer task, whose own timing is real-wall-clock and would
    /// race with the synthetic clock this drives) with a constant burst
    /// offered at 1.5x a fixed 1Mbps estimate, for one second of synthetic
    /// time at `BURST_TIME` ticks. Returns
    /// `(force_leak_ticks, drained_bits, offered_bits)`.
    fn run_pacer_burst(pacing_factor: f64) -> (usize, u64, u64) {
        let _lock = RIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        gst::init().unwrap();

        const ESTIMATE: u32 = 1_000_000;
        const PACKET_SIZE: usize = 1200;
        const TICKS: u32 = 200; // 1s of synthetic time at BURST_TIME (5ms) ticks
        let offered_bps = ESTIMATE as f64 * 1.5;

        let bwe = glib::Object::new::<crate::gcc::BandwidthEstimator>();
        bwe.set_property("estimated-bitrate", ESTIMATE);
        bwe.set_property("min-bitrate", 100_000u32);
        bwe.set_property("max-bitrate", 8_192_000u32);
        bwe.set_property("pacing-factor", pacing_factor);

        let mut owed_bits = 0.0f64;
        let mut offered_bits_total = 0u64;
        let mut drained_bits_total = 0u64;
        for _ in 0..TICKS {
            owed_bits += offered_bps * super::super::BURST_TIME.whole_nanoseconds() as f64
                / 1_000_000_000.0;
            {
                let imp = bwe.imp();
                let mut state = imp.state.lock().unwrap();
                while owed_bits >= (PACKET_SIZE * 8) as f64 {
                    state.buffers.push_front(gst::Buffer::with_size(PACKET_SIZE).unwrap());
                    owed_bits -= (PACKET_SIZE * 8) as f64;
                    offered_bits_total += (PACKET_SIZE * 8) as u64;
                }
            }

            clock::advance_to(clock::stream() + super::super::BURST_TIME);

            let imp = bwe.imp();
            let mut state = imp.state.lock().unwrap();
            let list = state.create_buffer_list(&bwe);
            drained_bits_total += list.iter().map(|b| b.size() as u64 * 8).sum::<u64>();
        }

        let imp = bwe.imp();
        let leaks = imp.state.lock().unwrap().force_leak_count;
        (leaks, drained_bits_total, offered_bits_total)
    }
}
