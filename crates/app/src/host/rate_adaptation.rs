//! The host side of bitrate adaptation: every
//! [`pipeline::bitrate::SAMPLE_INTERVAL`] it turns what the transport, the
//! send queue and the viewer's reports show into a [`NetworkSample`] for the
//! [`BitrateController`], and has the encoder follow its target.
//!
//! An encoder that refuses a new rate ends the adaptation for the session:
//! it is reported once and the stream stays at the rate it had.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use anyhow::Result;
use net::TransportStats;
use pipeline::bitrate::SAMPLE_INTERVAL;
use pipeline::{BitrateController, NetworkSample};
use protocol::feedback::ReceptionReport;

/// The viewer's reports not sampled yet, summed. Written by the session's
/// control loop, drained by the streaming thread.
#[derive(Debug, Default)]
pub(super) struct ViewerReports {
    frames_lost: AtomicU32,
    frames_late: AtomicU32,
}

impl ViewerReports {
    pub(super) fn record(&self, report: &ReceptionReport) {
        add_saturating(&self.frames_lost, report.frames_lost);
        add_saturating(&self.frames_late, report.frames_late);
    }

    /// Everything recorded since the previous call.
    fn take(&self) -> ReceptionReport {
        ReceptionReport {
            frames_lost: self.frames_lost.swap(0, Ordering::Relaxed),
            frames_late: self.frames_late.swap(0, Ordering::Relaxed),
        }
    }
}

fn add_saturating(counter: &AtomicU32, value: u32) {
    // The closure always returns `Some`, so the update cannot fail.
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(value))
    });
}

/// Cumulative counters of the stream, read when a sample is due.
#[derive(Debug, Clone, Copy)]
pub(super) struct Counters {
    pub(super) transport: TransportStats,
    /// `HostStats::frames_skipped`: frames not encoded for a full queue.
    pub(super) frames_skipped: u64,
}

/// What a due sample led to.
#[derive(Debug)]
pub(super) enum RateChange {
    /// The encoder now aims at this bitrate.
    Retargeted(u32),
    /// The encoder refused it: the rate stays fixed from now on.
    Fixed(anyhow::Error),
}

/// Adapts one session's bitrate; see the module documentation.
pub(super) struct RateAdaptation {
    controller: BitrateController,
    reports: Arc<ViewerReports>,
    previous: Counters,
    next_sample: Instant,
    /// False once the encoder refused a rate.
    active: bool,
}

impl RateAdaptation {
    /// Starts at `max_bps`, the configured bitrate; `counters` are the
    /// stream's counters now, the base of the first sample.
    pub(super) fn new(
        max_bps: u32,
        reports: Arc<ViewerReports>,
        counters: Counters,
        now: Instant,
    ) -> Self {
        Self {
            controller: BitrateController::new(max_bps, now),
            reports,
            previous: counters,
            next_sample: now + SAMPLE_INTERVAL,
            active: true,
        }
    }

    /// Moves the ceiling to `max_bps` (the sound started or stopped taking
    /// its share, see [`pipeline::video_ceiling`]). A target above it comes
    /// down at once and is handed to `apply` (the encoder).
    pub(super) fn set_ceiling(
        &mut self,
        max_bps: u32,
        apply: impl FnOnce(u32) -> Result<()>,
    ) -> Option<RateChange> {
        if !self.active {
            return None;
        }
        let target = self.controller.set_max(max_bps)?;
        Some(self.apply(target, apply))
    }

    /// Cheap enough for every loop iteration: until a sample is due it
    /// reads nothing. When one is, samples `counters()` and the viewer's
    /// reports and, if the target changed, hands it to `apply` (the
    /// encoder).
    pub(super) fn poll(
        &mut self,
        now: Instant,
        counters: impl FnOnce() -> Counters,
        apply: impl FnOnce(u32) -> Result<()>,
    ) -> Option<RateChange> {
        if !self.active || now < self.next_sample {
            return None;
        }
        self.next_sample = now + SAMPLE_INTERVAL;
        let current = counters();
        let sample = network_sample(&self.previous, &current, self.reports.take());
        self.previous = current;
        let target = self.controller.on_sample(&sample, now)?;
        Some(self.apply(target, apply))
    }

    /// Has the encoder aim at `target`; one that refuses ends the adaptation.
    fn apply(&mut self, target: u32, apply: impl FnOnce(u32) -> Result<()>) -> RateChange {
        match apply(target) {
            Ok(()) => RateChange::Retargeted(target),
            Err(error) => {
                self.active = false;
                RateChange::Fixed(error)
            }
        }
    }
}

/// The interval between two readings of the counters.
fn network_sample(before: &Counters, after: &Counters, viewer: ReceptionReport) -> NetworkSample {
    let (old, new) = (&before.transport, &after.transport);
    NetworkSample {
        packets_sent: new.sent_packets.saturating_sub(old.sent_packets),
        packets_lost: new.lost_packets.saturating_sub(old.lost_packets),
        frames_skipped: after.frames_skipped.saturating_sub(before.frames_skipped),
        viewer,
        rtt: new.rtt,
        min_rtt: new.min_rtt,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anyhow::anyhow;
    use pipeline::bitrate::DECREASE_FACTOR;

    use super::*;

    const MAX: u32 = 20_000_000;
    const CUT: u32 = (MAX as f64 * DECREASE_FACTOR) as u32;

    fn counters(sent: u64, lost: u64, skipped: u64) -> Counters {
        Counters {
            transport: TransportStats {
                rtt: Duration::from_millis(2),
                min_rtt: Duration::from_millis(1),
                congestion_window: 100_000,
                lost_packets: lost,
                sent_packets: sent,
            },
            frames_skipped: skipped,
        }
    }

    fn accept(_: u32) -> Result<()> {
        Ok(())
    }

    #[test]
    fn reports_add_up_until_taken() {
        let reports = ViewerReports::default();
        let report = |frames_lost, frames_late| ReceptionReport {
            frames_lost,
            frames_late,
        };
        reports.record(&report(1, 2));
        reports.record(&report(3, 4));
        assert_eq!(reports.take(), report(4, 6));
        assert_eq!(reports.take(), ReceptionReport::default());
        reports.record(&report(u32::MAX, 0));
        reports.record(&report(5, 0));
        assert_eq!(reports.take().frames_lost, u32::MAX);
    }

    #[test]
    fn samples_are_the_difference_of_the_counters() {
        let viewer = ReceptionReport {
            frames_lost: 1,
            frames_late: 2,
        };
        let sample = network_sample(&counters(100, 1, 3), &counters(350, 6, 4), viewer);
        assert_eq!(sample.packets_sent, 250);
        assert_eq!(sample.packets_lost, 5);
        assert_eq!(sample.frames_skipped, 1);
        assert_eq!(sample.viewer, viewer);
        assert_eq!(sample.rtt, Duration::from_millis(2));
        assert_eq!(sample.min_rtt, Duration::from_millis(1));
    }

    #[test]
    fn nothing_is_read_before_a_sample_is_due() {
        let start = Instant::now();
        let mut rate = RateAdaptation::new(MAX, Arc::default(), counters(0, 0, 0), start);
        let early = start + SAMPLE_INTERVAL / 2;
        let change = rate.poll(early, || panic!("read too early"), accept);
        assert!(change.is_none());
    }

    #[test]
    fn losses_reported_by_the_viewer_lower_the_encoder_rate() {
        let start = Instant::now();
        let reports = Arc::new(ViewerReports::default());
        let mut rate = RateAdaptation::new(MAX, reports.clone(), counters(0, 0, 0), start);
        reports.record(&ReceptionReport {
            frames_lost: 5,
            frames_late: 0,
        });
        let mut applied = None;
        let change = rate.poll(
            start + SAMPLE_INTERVAL,
            || counters(1_000, 0, 0),
            |bps| {
                applied = Some(bps);
                Ok(())
            },
        );
        assert!(matches!(change, Some(RateChange::Retargeted(CUT))));
        assert_eq!(applied, Some(CUT));
    }

    #[test]
    fn transport_losses_lower_the_rate_and_calm_brings_it_back() {
        let start = Instant::now();
        let mut rate = RateAdaptation::new(MAX, Arc::default(), counters(0, 0, 0), start);
        let mut now = start + SAMPLE_INTERVAL;
        let change = rate.poll(now, || counters(1_000, 100, 0), accept);
        assert!(matches!(change, Some(RateChange::Retargeted(CUT))));
        let mut target = CUT;
        for _ in 0..1_000 {
            now += SAMPLE_INTERVAL;
            if let Some(RateChange::Retargeted(bps)) =
                rate.poll(now, || counters(1_000, 100, 0), accept)
            {
                assert!(bps > target && bps <= MAX, "{bps} after {target}");
                target = bps;
            }
        }
        assert_eq!(target, MAX);
    }

    #[test]
    fn the_sound_lowers_the_ceiling_and_the_target_with_it() {
        let start = Instant::now();
        let mut rate = RateAdaptation::new(MAX, Arc::default(), counters(0, 0, 0), start);
        let lower = pipeline::video_ceiling(MAX, 1_700_000);
        let mut applied = None;
        let change = rate.set_ceiling(lower, |bps| {
            applied = Some(bps);
            Ok(())
        });
        assert!(matches!(change, Some(RateChange::Retargeted(bps)) if bps == lower));
        assert_eq!(applied, Some(lower));
        assert!(rate.set_ceiling(lower, accept).is_none(), "nothing changed");
        assert!(
            rate.set_ceiling(MAX, accept).is_none(),
            "climbed back later"
        );
    }

    #[test]
    fn a_refused_ceiling_fixes_the_rate_like_a_refused_sample() {
        let start = Instant::now();
        let mut rate = RateAdaptation::new(MAX, Arc::default(), counters(0, 0, 0), start);
        let change = rate.set_ceiling(MAX / 2, |_| Err(anyhow!("refused")));
        assert!(matches!(change, Some(RateChange::Fixed(_))));
        assert!(rate.set_ceiling(MAX / 4, accept).is_none());
    }

    #[test]
    fn a_refused_rate_is_reported_once_and_adaptation_stops() {
        let start = Instant::now();
        let mut rate = RateAdaptation::new(MAX, Arc::default(), counters(0, 0, 0), start);
        let mut now = start;
        let mut changes = Vec::new();
        for sent in 1..20 {
            now += SAMPLE_INTERVAL;
            let lossy = counters(sent * 1_000, sent * 100, 0);
            changes.extend(rate.poll(now, || lossy, |_| Err(anyhow!("refused"))));
        }
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], RateChange::Fixed(e) if e.to_string() == "refused"));
    }
}
