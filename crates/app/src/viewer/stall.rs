//! Notices that the picture stopped. Pure: the render loop reports every
//! datagram and asks, on every turn, what to do about the silence.
//!
//! A static screen sends no video at all, so silence alone does not mean a
//! broken connection. After [`PROBE_AFTER`] the viewer therefore asks the
//! host for a keyframe: a host that is alive answers at once, even for an
//! unchanged screen, and only a connection that really stalled stays silent
//! until [`UNSTABLE_AFTER`].

use std::time::{Duration, Instant};

/// Silence after which the host is asked for a keyframe (and again every
/// this long while it stays silent).
pub const PROBE_AFTER: Duration = Duration::from_secs(1);
/// Silence after which the user is told the connection is unstable.
pub const UNSTABLE_AFTER: Duration = Duration::from_millis(1500);

/// What to do about the silence at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    /// Ask the host for a keyframe, to see if it is still there.
    pub probe: bool,
    /// Tell the user the picture is stuck.
    pub unstable: bool,
}

#[derive(Debug)]
pub struct StallWatch {
    last_data: Instant,
    last_probe: Option<Instant>,
}

impl StallWatch {
    /// Starts watching at `now`, the moment the video is expected to begin.
    pub fn new(now: Instant) -> Self {
        Self {
            last_data: now,
            last_probe: None,
        }
    }

    /// A video datagram arrived: the silence is over.
    pub fn on_datagram(&mut self, now: Instant) {
        self.last_data = now;
        self.last_probe = None;
    }

    pub fn check(&mut self, now: Instant) -> Verdict {
        let silence = now.saturating_duration_since(self.last_data);
        let due = self
            .last_probe
            .is_none_or(|probe| now.saturating_duration_since(probe) >= PROBE_AFTER);
        let probe = silence >= PROBE_AFTER && due;
        if probe {
            self.last_probe = Some(now);
        }
        Verdict {
            probe,
            unstable: silence > UNSTABLE_AFTER,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    const QUIET: Verdict = Verdict {
        probe: false,
        unstable: false,
    };

    #[test]
    fn a_steady_stream_is_never_questioned() {
        let base = Instant::now();
        let mut watch = StallWatch::new(base);
        for ms in (0..5000).step_by(100) {
            watch.on_datagram(at(base, ms));
            assert_eq!(watch.check(at(base, ms + 50)), QUIET);
        }
    }

    #[test]
    fn silence_is_probed_after_a_second_and_reported_after_one_and_a_half() {
        let base = Instant::now();
        let mut watch = StallWatch::new(base);
        assert_eq!(watch.check(at(base, 999)), QUIET);
        assert_eq!(
            watch.check(at(base, 1000)),
            Verdict {
                probe: true,
                unstable: false
            }
        );
        assert_eq!(
            watch.check(at(base, 1500)),
            QUIET,
            "exactly 1.5 s is not yet more than 1.5 s"
        );
        assert_eq!(watch.check(at(base, 1501)), STUCK);
    }

    const STUCK: Verdict = Verdict {
        probe: false,
        unstable: true,
    };

    #[test]
    fn the_probe_repeats_every_second_of_silence() {
        let base = Instant::now();
        let mut watch = StallWatch::new(base);
        let probes: Vec<u64> = (0..=3500)
            .step_by(50)
            .filter(|&ms| watch.check(at(base, ms)).probe)
            .collect();
        assert_eq!(probes, [1000, 2000, 3000]);
    }

    #[test]
    fn a_datagram_ends_the_warning_at_once() {
        let base = Instant::now();
        let mut watch = StallWatch::new(base);
        assert!(watch.check(at(base, 2000)).unstable);
        watch.on_datagram(at(base, 2010));
        assert_eq!(watch.check(at(base, 2020)), QUIET);
    }

    #[test]
    fn a_new_silence_is_probed_again_after_a_second() {
        let base = Instant::now();
        let mut watch = StallWatch::new(base);
        assert!(watch.check(at(base, 1000)).probe);
        watch.on_datagram(at(base, 1100));
        assert!(!watch.check(at(base, 2000)).probe);
        assert!(watch.check(at(base, 2100)).probe);
    }

    #[test]
    fn a_clock_that_runs_backwards_is_harmless() {
        let base = Instant::now();
        let mut watch = StallWatch::new(at(base, 1000));
        assert_eq!(watch.check(base), QUIET);
    }
}
