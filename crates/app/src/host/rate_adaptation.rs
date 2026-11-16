//! The host side of bitrate adaptation: every
//! [`pipeline::bitrate::SAMPLE_INTERVAL`] it turns what the transport, the
//! send queue and the viewer's reports show into a [`NetworkSample`] for the
//! [`BitrateController`], and has the encoder follow its target.
//!
//! The adaptation outlives the encoder: when the stream starts over (another
//! monitor or quality, a new resolution) the new encoder starts from the
//! rate the network settled at, not from the maximum ([`RateAdaptation::resume`]).
//!
//! An encoder that refuses a new rate ends the adaptation until the stream
//! starts over: it is reported once and the stream stays at the rate it had.

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

    /// The stream started over with a new encoder, whose ceiling is
    /// `max_bps`: it is handed (`apply`) the target the network settled at,
    /// brought down to `max_bps` if above it, rather than starting from the
    /// maximum. A higher ceiling is climbed to as usual. The new encoder gets
    /// its chance even if the previous one refused a rate.
    pub(super) fn resume(
        &mut self,
        max_bps: u32,
        apply: impl FnOnce(u32) -> Result<()>,
    ) -> RateChange {
        self.active = true;
        self.controller.set_max(max_bps);
        let target = self.controller.target_bps();
        self.apply(target, apply)
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
        file_transfers: old.file_streams_open > 0
            || new.file_streams_open > 0
            || new.file_streams_opened > old.file_streams_opened,
    }
}

#[cfg(test)]
mod tests;
