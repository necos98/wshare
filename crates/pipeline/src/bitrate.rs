//! Fitting the encoder's bitrate to the network (AIMD with hysteresis).
//!
//! A stream faster than the path queues up on the way: first the picture
//! lags, then datagrams are lost and the viewer freezes until a keyframe.
//! The host therefore samples what the network shows every
//! [`SAMPLE_INTERVAL`] ([`NetworkSample`]) and steers the encoder's target:
//!
//! - congestion (losses, a send backlog, frames arriving late, a growing
//!   RTT): multiply by [`DECREASE_FACTOR`] at once, then wait
//!   [`DECREASE_HOLD`] before cutting again, so the effects of the old rate
//!   still on the way do not count twice;
//! - calm for [`INCREASE_DELAY`]: add [`INCREASE_PER_SECOND`] of the
//!   maximum per second, probing slowly for the bandwidth back.
//!
//! The target always stays between [`MIN_BITRATE_BPS`] (or the maximum, if
//! lower) and the maximum: the bitrate the user configured.

use std::time::{Duration, Instant};

use protocol::feedback::ReceptionReport;

/// How often the host evaluates the network. Several samples fit in the
/// second within which a congestion must be answered.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// Floor of the target: below it the picture is too poor to be worth
/// keeping fluid.
pub const MIN_BITRATE_BPS: u32 = 1_500_000;

/// Multiplicative decrease applied on congestion.
pub const DECREASE_FACTOR: f64 = 0.7;

/// Minimum time between two decreases: about the time a lower rate needs
/// to show in the viewer's reports.
pub const DECREASE_HOLD: Duration = Duration::from_millis(750);

/// Calm needed after the last congestion before the target grows again.
pub const INCREASE_DELAY: Duration = Duration::from_secs(2);

/// Additive increase, as a fraction of the maximum per second of calm.
pub const INCREASE_PER_SECOND: f64 = 0.05;

/// Longest interval one increase accounts for, so a sample arriving late
/// (e.g. after a stalled thread) cannot jump the target up.
const MAX_INCREASE_STEP: Duration = Duration::from_secs(1);

/// Share of lost packets above which the path counts as congested. Wi-Fi
/// loses the odd packet even when idle; that alone is no reason to cut.
const LOSS_RATIO: f64 = 0.02;

/// Fewest lost packets that count, so a single loss in a quiet interval
/// (a ratio of 100%) does not.
const MIN_LOST_PACKETS: u64 = 2;

/// Frames not encoded because the send queue was full: one can be a
/// keyframe's burst, more mean the path is slower than the stream.
const SKIPPED_FRAMES: u64 = 2;

/// Frames the viewer reported lost in one sample.
const VIEWER_LOST_FRAMES: u32 = 2;

/// Frames the viewer reported late in one sample: a few happen when its
/// own rendering hiccups; more mean video is queueing on the way.
const VIEWER_LATE_FRAMES: u32 = 3;

/// Queueing delay (smoothed RTT above the path's minimum) that counts as a
/// growing queue.
const RTT_RISE: Duration = Duration::from_millis(25);

/// What the network showed during one sample interval. Counts are for the
/// interval only; RTTs are the transport's current estimates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetworkSample {
    pub packets_sent: u64,
    pub packets_lost: u64,
    /// Frames the host skipped because the send backlog was over its limit.
    pub frames_skipped: u64,
    /// The viewer's reports received during the interval, summed.
    pub viewer: ReceptionReport,
    pub rtt: Duration,
    pub min_rtt: Duration,
}

impl NetworkSample {
    /// Whether anything in the sample says the stream is too fast.
    pub fn congested(&self) -> bool {
        self.lossy()
            || self.frames_skipped >= SKIPPED_FRAMES
            || self.viewer.frames_lost >= VIEWER_LOST_FRAMES
            || self.viewer.frames_late >= VIEWER_LATE_FRAMES
            || self.rtt.saturating_sub(self.min_rtt) > RTT_RISE
    }

    fn lossy(&self) -> bool {
        self.packets_lost >= MIN_LOST_PACKETS
            && self.packets_lost as f64 > self.packets_sent as f64 * LOSS_RATIO
    }
}

/// The share of `max_bps`, the bitrate the user configured, left to the
/// video while `reserved_bps` go to other traffic on the same path (the
/// audio). Never below the floor, or `max_bps` itself if that is lower.
pub fn video_ceiling(max_bps: u32, reserved_bps: u32) -> u32 {
    max_bps
        .saturating_sub(reserved_bps)
        .max(MIN_BITRATE_BPS.min(max_bps))
}

/// The adaptive target bitrate. Pure: time comes in with every sample.
#[derive(Debug, Clone)]
pub struct BitrateController {
    min_bps: u32,
    max_bps: u32,
    target_bps: u32,
    last_decrease: Option<Instant>,
    /// Last congestion seen (or the start): increases wait for calm.
    last_congestion: Instant,
    last_sample: Instant,
}

impl BitrateController {
    /// Starts at `max_bps`, the configured bitrate, which the target never
    /// exceeds.
    pub fn new(max_bps: u32, now: Instant) -> Self {
        Self {
            min_bps: MIN_BITRATE_BPS.min(max_bps),
            max_bps,
            target_bps: max_bps,
            last_decrease: None,
            last_congestion: now,
            last_sample: now,
        }
    }

    pub fn target_bps(&self) -> u32 {
        self.target_bps
    }

    /// Moves the ceiling to `max_bps` (e.g. the audio started or stopped,
    /// see [`video_ceiling`]). A lower ceiling brings the target down at
    /// once, which is returned; a higher one is reached by the usual
    /// increases, as after a congestion.
    pub fn set_max(&mut self, max_bps: u32) -> Option<u32> {
        self.max_bps = max_bps;
        self.min_bps = MIN_BITRATE_BPS.min(max_bps);
        let next = self.target_bps.clamp(self.min_bps, self.max_bps);
        (next != self.target_bps).then(|| {
            self.target_bps = next;
            next
        })
    }

    /// Takes the sample of the interval ending at `now`; returns the new
    /// target when it changed.
    pub fn on_sample(&mut self, sample: &NetworkSample, now: Instant) -> Option<u32> {
        let elapsed = now.saturating_duration_since(self.last_sample);
        self.last_sample = now;
        let next = if sample.congested() {
            self.last_congestion = now;
            self.decreased(now)
        } else {
            self.increased(now, elapsed)
        };
        (next != self.target_bps).then(|| {
            self.target_bps = next;
            next
        })
    }

    fn decreased(&mut self, now: Instant) -> u32 {
        let holding = self
            .last_decrease
            .is_some_and(|last| now.saturating_duration_since(last) < DECREASE_HOLD);
        if holding || self.target_bps == self.min_bps {
            return self.target_bps;
        }
        self.last_decrease = Some(now);
        let cut = (f64::from(self.target_bps) * DECREASE_FACTOR) as u32;
        cut.max(self.min_bps)
    }

    fn increased(&self, now: Instant, elapsed: Duration) -> u32 {
        if now.saturating_duration_since(self.last_congestion) < INCREASE_DELAY {
            return self.target_bps;
        }
        let seconds = elapsed.min(MAX_INCREASE_STEP).as_secs_f64();
        let step = (f64::from(self.max_bps) * INCREASE_PER_SECOND * seconds) as u32;
        self.target_bps.saturating_add(step).min(self.max_bps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: u32 = 20_000_000;

    fn calm() -> NetworkSample {
        NetworkSample {
            packets_sent: 1_000,
            rtt: Duration::from_millis(2),
            min_rtt: Duration::from_millis(1),
            ..Default::default()
        }
    }

    fn lossy() -> NetworkSample {
        NetworkSample {
            packets_lost: 100,
            ..calm()
        }
    }

    /// Drives a controller with one sample every [`SAMPLE_INTERVAL`].
    struct Clock {
        controller: BitrateController,
        start: Instant,
        now: Instant,
    }

    impl Clock {
        fn new(max_bps: u32) -> Self {
            let start = Instant::now();
            Self {
                controller: BitrateController::new(max_bps, start),
                start,
                now: start,
            }
        }

        fn sample(&mut self, sample: NetworkSample) -> u32 {
            self.now += SAMPLE_INTERVAL;
            self.controller.on_sample(&sample, self.now);
            self.controller.target_bps()
        }

        /// Samples `sample` for `duration`; returns the targets seen.
        fn run(&mut self, sample: NetworkSample, duration: Duration) -> Vec<u32> {
            let end = self.now + duration;
            let mut targets = Vec::new();
            while self.now < end {
                targets.push(self.sample(sample));
            }
            targets
        }

        fn elapsed(&self) -> Duration {
            self.now - self.start
        }
    }

    #[test]
    fn the_video_gets_what_the_audio_leaves_down_to_the_floor() {
        assert_eq!(video_ceiling(MAX, 1_700_000), MAX - 1_700_000);
        assert_eq!(video_ceiling(MAX, 0), MAX);
        assert_eq!(video_ceiling(2_000_000, 1_700_000), MIN_BITRATE_BPS);
        assert_eq!(video_ceiling(1_000_000, 1_700_000), 1_000_000);
    }

    #[test]
    fn a_lower_ceiling_cuts_the_target_at_once_a_higher_one_is_climbed_to() {
        let mut clock = Clock::new(MAX);
        let lower = video_ceiling(MAX, 1_700_000);
        assert_eq!(clock.controller.set_max(lower), Some(lower));
        assert_eq!(clock.controller.set_max(lower), None);
        assert!(
            clock
                .run(calm(), Duration::from_secs(10))
                .iter()
                .all(|&t| t == lower)
        );
        assert_eq!(clock.controller.set_max(MAX), None, "no jump up");
        let targets = clock.run(calm(), Duration::from_secs(10));
        assert_eq!(*targets.last().unwrap(), MAX);
        assert!(targets.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn starts_at_the_configured_maximum() {
        assert_eq!(
            BitrateController::new(MAX, Instant::now()).target_bps(),
            MAX
        );
    }

    #[test]
    fn a_calm_network_keeps_the_maximum() {
        let mut clock = Clock::new(MAX);
        let targets = clock.run(calm(), Duration::from_secs(10));
        assert!(targets.iter().all(|&t| t == MAX));
    }

    #[test]
    fn a_burst_of_losses_cuts_the_rate_at_the_first_sample() {
        let mut clock = Clock::new(MAX);
        assert_eq!(clock.sample(lossy()), (MAX as f64 * DECREASE_FACTOR) as u32);
    }

    #[test]
    fn decreases_are_spaced_by_the_hold_time() {
        let mut clock = Clock::new(MAX);
        let first = clock.sample(lossy());
        // Within the hold the cut is not repeated.
        assert_eq!(clock.sample(lossy()), first);
        assert_eq!(clock.sample(lossy()), first);
        // Then congestion that goes on cuts again.
        let second = clock.sample(lossy());
        assert_eq!(second, (first as f64 * DECREASE_FACTOR) as u32);
    }

    #[test]
    fn congestion_halves_the_rate_within_about_a_second() {
        let mut clock = Clock::new(MAX);
        let targets = clock.run(lossy(), Duration::from_secs(1));
        assert!(*targets.last().unwrap() <= MAX / 2, "{targets:?}");
    }

    #[test]
    fn lasting_congestion_stops_at_the_floor() {
        let mut clock = Clock::new(MAX);
        let targets = clock.run(lossy(), Duration::from_secs(30));
        assert!(targets.iter().all(|&t| t >= MIN_BITRATE_BPS));
        assert_eq!(*targets.last().unwrap(), MIN_BITRATE_BPS);
    }

    #[test]
    fn calm_after_congestion_waits_then_climbs_back_to_the_ceiling() {
        let mut clock = Clock::new(MAX);
        clock.run(lossy(), Duration::from_secs(10));
        let low = clock.controller.target_bps();
        assert_eq!(low, MIN_BITRATE_BPS);

        // No increase until the network has been calm for a while.
        let waiting = clock.run(calm(), INCREASE_DELAY - SAMPLE_INTERVAL);
        assert!(waiting.iter().all(|&t| t == low), "{waiting:?}");

        // Then a gradual climb: about 5% of the maximum per second.
        let climbing = clock.run(calm(), Duration::from_secs(1));
        let per_second = climbing.last().unwrap() - low;
        let expected = (MAX as f64 * INCREASE_PER_SECOND) as u32;
        assert!(
            per_second.abs_diff(expected) <= expected / 10,
            "+{per_second} in a second, expected about +{expected}"
        );
        assert!(climbing.windows(2).all(|w| w[0] <= w[1]));

        // Until the ceiling, never above it.
        let later = clock.run(calm(), Duration::from_secs(30));
        assert!(later.iter().all(|&t| t <= MAX));
        assert_eq!(*later.last().unwrap(), MAX);
    }

    #[test]
    fn congestion_during_the_climb_restarts_the_wait() {
        let mut clock = Clock::new(MAX);
        clock.sample(lossy());
        clock.run(calm(), Duration::from_secs(4));
        let climbed = clock.controller.target_bps();
        let cut = clock.sample(lossy());
        assert!(cut < climbed);
        let waiting = clock.run(calm(), INCREASE_DELAY - SAMPLE_INTERVAL);
        assert!(waiting.iter().all(|&t| t == cut));
    }

    #[test]
    fn a_late_sample_does_not_jump_the_target() {
        let mut clock = Clock::new(MAX);
        clock.sample(lossy());
        clock.run(calm(), INCREASE_DELAY);
        let before = clock.controller.target_bps();
        clock.now += Duration::from_secs(60);
        let after = clock.sample(calm());
        let largest_step = (MAX as f64 * INCREASE_PER_SECOND) as u32;
        assert!(after - before <= largest_step, "+{}", after - before);
    }

    #[test]
    fn a_maximum_below_the_floor_is_kept_fixed() {
        let mut clock = Clock::new(1_000_000);
        assert_eq!(clock.sample(lossy()), 1_000_000);
        assert_eq!(clock.controller.on_sample(&lossy(), clock.now), None);
        assert_eq!(clock.run(calm(), Duration::from_secs(5)), [1_000_000; 20]);
    }

    #[test]
    fn only_changes_are_reported() {
        let start = Instant::now();
        let mut controller = BitrateController::new(MAX, start);
        assert_eq!(controller.on_sample(&calm(), start + SAMPLE_INTERVAL), None);
        let cut = controller.on_sample(&lossy(), start + SAMPLE_INTERVAL * 2);
        assert_eq!(cut, Some(controller.target_bps()));
    }

    #[test]
    fn mixed_conditions_stay_within_floor_and_ceiling() {
        let mut clock = Clock::new(MAX);
        // Deterministic pseudo-random sequence of calm and congested samples.
        let mut seed: u32 = 12_345;
        for _ in 0..2_000 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let sample = if (seed >> 16).is_multiple_of(4) {
                lossy()
            } else {
                calm()
            };
            let target = clock.sample(sample);
            assert!((MIN_BITRATE_BPS..=MAX).contains(&target), "{target}");
        }
        assert!(clock.elapsed() > Duration::from_secs(60));
    }

    #[test]
    fn each_signal_alone_means_congestion() {
        let congested = [
            NetworkSample {
                packets_lost: 30,
                ..calm()
            },
            NetworkSample {
                frames_skipped: SKIPPED_FRAMES,
                ..calm()
            },
            NetworkSample {
                viewer: ReceptionReport {
                    frames_lost: VIEWER_LOST_FRAMES,
                    frames_late: 0,
                },
                ..calm()
            },
            NetworkSample {
                viewer: ReceptionReport {
                    frames_lost: 0,
                    frames_late: VIEWER_LATE_FRAMES,
                },
                ..calm()
            },
            NetworkSample {
                rtt: calm().min_rtt + RTT_RISE + Duration::from_millis(1),
                ..calm()
            },
        ];
        for sample in congested {
            assert!(sample.congested(), "{sample:?}");
        }
    }

    #[test]
    fn noise_below_the_thresholds_is_calm() {
        let calm_enough = [
            calm(),
            NetworkSample::default(),
            // 1% loss, as Wi-Fi shows when idle.
            NetworkSample {
                packets_lost: 10,
                ..calm()
            },
            // A single lost packet in a quiet interval.
            NetworkSample {
                packets_sent: 1,
                packets_lost: 1,
                ..calm()
            },
            NetworkSample {
                frames_skipped: SKIPPED_FRAMES - 1,
                viewer: ReceptionReport {
                    frames_lost: VIEWER_LOST_FRAMES - 1,
                    frames_late: VIEWER_LATE_FRAMES - 1,
                },
                rtt: calm().min_rtt + RTT_RISE,
                ..calm()
            },
        ];
        for sample in calm_enough {
            assert!(!sample.congested(), "{sample:?}");
        }
    }
}
