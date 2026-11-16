//! The viewer's part in the host's bitrate adaptation: it reports how the
//! video arrives and shows the rate the host aims at.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use pipeline::ViewerStats;
use protocol::feedback::{RECEPTION_REPORT_INTERVAL, ReceptionReport};

/// Turns the cumulative viewer statistics into a [`ReceptionReport`] every
/// [`RECEPTION_REPORT_INTERVAL`], counting what happened in between.
pub struct ReceptionReporter {
    next: Instant,
    lost: u64,
    late: u64,
}

impl ReceptionReporter {
    /// The first report comes one interval after `now`.
    pub fn new(now: Instant) -> Self {
        Self {
            next: now + RECEPTION_REPORT_INTERVAL,
            lost: 0,
            late: 0,
        }
    }

    /// The report due at `now`, if one is. Late frames are the pictures
    /// passed over because a newer one arrived with them, except those a
    /// recovered frame released at once ([`ViewerStats::frames_late`]): a
    /// resend that succeeded is no sign of congestion.
    pub fn poll(&mut self, now: Instant, stats: &ViewerStats) -> Option<ReceptionReport> {
        if now < self.next {
            return None;
        }
        self.next = now + RECEPTION_REPORT_INTERVAL;
        let report = ReceptionReport {
            frames_lost: since(stats.frames_lost, self.lost),
            frames_late: since(stats.frames_late, self.late),
        };
        self.lost = stats.frames_lost;
        self.late = stats.frames_late;
        Some(report)
    }
}

/// How much `total` grew since `before`, saturating at `u32::MAX`.
fn since(total: u64, before: u64) -> u32 {
    u32::try_from(total.saturating_sub(before)).unwrap_or(u32::MAX)
}

/// The bitrate the host last announced (`TargetBitrate`), shared between
/// the control task that receives it and the render thread that shows it.
#[derive(Debug, Clone, Default)]
pub struct TargetRate(Arc<AtomicU32>);

/// Stored before the host announced anything; a real target is never 0.
const UNKNOWN: u32 = 0;

impl TargetRate {
    pub fn set(&self, bps: u32) {
        self.0.store(bps, Ordering::Relaxed);
    }

    /// `None` until the host announced a target.
    pub fn get(&self) -> Option<u32> {
        Some(self.0.load(Ordering::Relaxed)).filter(|&bps| bps != UNKNOWN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(frames_lost: u64, frames_late: u64) -> ViewerStats {
        ViewerStats {
            frames_lost,
            frames_late,
            ..Default::default()
        }
    }

    #[test]
    fn a_report_is_due_every_interval_with_the_counts_in_between() {
        let start = Instant::now();
        let mut reporter = ReceptionReporter::new(start);
        assert_eq!(reporter.poll(start, &stats(1, 1)), None);
        let first = start + RECEPTION_REPORT_INTERVAL;
        assert_eq!(
            reporter.poll(first, &stats(2, 5)),
            Some(ReceptionReport {
                frames_lost: 2,
                frames_late: 5,
            })
        );
        assert_eq!(reporter.poll(first, &stats(9, 9)), None);
        let second = first + RECEPTION_REPORT_INTERVAL;
        assert_eq!(
            reporter.poll(second, &stats(3, 5)),
            Some(ReceptionReport {
                frames_lost: 1,
                frames_late: 0,
            })
        );
    }

    #[test]
    fn a_quiet_interval_reports_zero() {
        let start = Instant::now();
        let mut reporter = ReceptionReporter::new(start);
        let due = start + RECEPTION_REPORT_INTERVAL;
        assert_eq!(
            reporter.poll(due, &stats(0, 0)),
            Some(ReceptionReport::default())
        );
    }

    #[test]
    fn frames_a_recovery_released_are_not_reported_late() {
        let start = Instant::now();
        let mut reporter = ReceptionReporter::new(start);
        let after_recoveries = ViewerStats {
            frames_superseded: 6,
            frames_recovered: 2,
            ..Default::default()
        };
        let due = start + RECEPTION_REPORT_INTERVAL;
        assert_eq!(
            reporter.poll(due, &after_recoveries),
            Some(ReceptionReport::default())
        );
    }

    #[test]
    fn huge_counts_saturate() {
        assert_eq!(since(u64::MAX, 0), u32::MAX);
        assert_eq!(since(1, 5), 0);
    }

    #[test]
    fn target_is_unknown_until_announced() {
        let target = TargetRate::default();
        let shared = target.clone();
        assert_eq!(target.get(), None);
        shared.set(12_000_000);
        assert_eq!(target.get(), Some(12_000_000));
    }
}
