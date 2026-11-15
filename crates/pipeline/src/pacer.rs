//! Decides when a captured frame should be encoded.
//!
//! Rules:
//! - Only encode when the screen changed ("dirty"); a static screen costs
//!   nothing.
//! - Never encode more often than the target fps: if the screen changes
//!   faster, intermediate images are skipped and the latest one is sent at
//!   the next slot (the latest image is never lost, only delayed to its slot).
//! - The very first frame may be sent immediately.
//! - Slots sit on a fixed grid (`slot += interval`), not at "last submit +
//!   interval": vsync jitter that delivers a frame a little early or late
//!   must not shift every following slot. A slot is honoured up to
//!   [`SLOT_TOLERANCE`] early, and the grid restarts from "now" when a frame
//!   is more than one interval late (or after an idle period).
//! - A due slot can be given up (e.g. the network is congested): the image
//!   stays pending and is retried one interval later, never in a busy loop.
//!
//! Time is passed in explicitly so the logic is deterministic in tests.

use std::time::{Duration, Instant};

/// How much earlier than its slot a frame may be submitted. Covers the vsync
/// jitter and the coarse wake-up granularity of the capture wait, either of
/// which would otherwise cost a whole interval of latency.
pub const SLOT_TOLERANCE: Duration = Duration::from_millis(2);

/// The tolerance never exceeds this fraction of the interval, so very high
/// rates still cannot run ahead of their grid.
const MAX_TOLERANCE_DIVISOR: u32 = 4;

pub struct FramePacer {
    interval: Duration,
    /// [`SLOT_TOLERANCE`], capped so it stays a fraction of the interval.
    tolerance: Duration,
    dirty: bool,
    /// Grid position of the next frame (`None`: right away).
    next_slot: Option<Instant>,
}

impl FramePacer {
    /// Panics if `fps` is 0.
    pub fn new(fps: u32) -> Self {
        assert!(fps > 0, "fps must be positive");
        let interval = Duration::from_secs(1) / fps;
        Self {
            interval,
            tolerance: SLOT_TOLERANCE.min(interval / MAX_TOLERANCE_DIVISOR),
            dirty: false,
            next_slot: None,
        }
    }

    /// Target time between two frames.
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// The screen changed (or a refresh is needed, e.g. keyframe request).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// How long the capture may block waiting for a change before the
    /// caller must come back and check [`Self::should_submit`]. When dirty,
    /// this is the time left until the next slot is within tolerance (zero
    /// if it already is); otherwise one interval. Non-zero whenever
    /// `should_submit` is false.
    pub fn capture_timeout(&self, now: Instant) -> Duration {
        if !self.dirty {
            return self.interval;
        }
        self.next_slot.map_or(Duration::ZERO, |slot| {
            slot.saturating_duration_since(now + self.tolerance)
        })
    }

    /// True when dirty and the next slot is reached, give or take
    /// [`SLOT_TOLERANCE`].
    pub fn should_submit(&self, now: Instant) -> bool {
        self.dirty
            && self
                .next_slot
                .is_none_or(|slot| now + self.tolerance >= slot)
    }

    /// Records that a frame was submitted at `now`; clears the dirty flag.
    pub fn on_submitted(&mut self, now: Instant) {
        self.advance_grid(now);
        self.dirty = false;
    }

    /// Records that the slot due at `now` was given up without submitting:
    /// the next attempt waits one interval, and the dirty flag stays so the
    /// latest image is still sent once a slot is used.
    pub fn on_skipped(&mut self, now: Instant) {
        self.advance_grid(now);
    }

    /// Moves to the next grid slot; when `now` is already past it the grid
    /// restarts from `now`.
    fn advance_grid(&mut self, now: Instant) {
        let next = self.next_slot.unwrap_or(now) + self.interval;
        self.next_slot = Some(if now > next {
            now + self.interval
        } else {
            next
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    #[should_panic]
    fn zero_fps_panics() {
        let _ = FramePacer::new(0);
    }

    #[test]
    fn interval_matches_fps() {
        assert_eq!(FramePacer::new(50).interval(), 20 * MS);
        assert_eq!(FramePacer::new(1).interval(), Duration::from_secs(1));
    }

    #[test]
    fn first_frame_is_immediate() {
        let now = Instant::now();
        let mut p = FramePacer::new(50);
        assert!(!p.should_submit(now));
        p.mark_dirty();
        assert!(p.is_dirty());
        assert!(p.should_submit(now));
        assert_eq!(p.capture_timeout(now), Duration::ZERO);
    }

    #[test]
    fn rate_is_capped_within_tolerance() {
        let t0 = Instant::now();
        let mut p = FramePacer::new(50);
        p.mark_dirty();
        p.on_submitted(t0);
        assert!(!p.is_dirty());
        p.mark_dirty();
        assert!(!p.should_submit(t0 + 5 * MS));
        assert_eq!(p.capture_timeout(t0 + 5 * MS), 13 * MS);
        assert!(!p.should_submit(t0 + 17 * MS));
        assert!(p.should_submit(t0 + 18 * MS));
        assert_eq!(p.capture_timeout(t0 + 18 * MS), Duration::ZERO);
        assert_eq!(p.capture_timeout(t0 + 25 * MS), Duration::ZERO);
    }

    #[test]
    fn timeout_is_never_zero_while_not_due() {
        let t0 = Instant::now();
        let mut p = FramePacer::new(60);
        p.mark_dirty();
        p.on_submitted(t0);
        p.mark_dirty();
        for micros in (0..20_000).step_by(250) {
            let now = t0 + Duration::from_micros(micros);
            assert_eq!(
                p.should_submit(now),
                p.capture_timeout(now).is_zero(),
                "at {micros} us"
            );
        }
    }

    #[test]
    fn fast_changes_keep_latest_for_next_slot() {
        let t0 = Instant::now();
        let mut p = FramePacer::new(50);
        p.mark_dirty();
        p.on_submitted(t0);
        // Several changes inside one interval: still one pending submission.
        for i in 1..5 {
            p.mark_dirty();
            assert!(!p.should_submit(t0 + i * 3 * MS));
        }
        assert!(p.should_submit(t0 + 20 * MS));
        p.on_submitted(t0 + 20 * MS);
        assert!(!p.should_submit(t0 + 40 * MS - 3 * MS));
    }

    #[test]
    fn early_frames_do_not_drift_the_grid() {
        let t0 = Instant::now();
        let mut p = FramePacer::new(50);
        p.mark_dirty();
        p.on_submitted(t0);
        // Frames land 1 ms early every time: the slots stay at 20, 40, 60 ms
        // instead of sliding earlier by 1 ms per frame.
        for slot in 1..=5u32 {
            p.mark_dirty();
            let early = t0 + slot * 20 * MS - MS;
            assert!(p.should_submit(early), "slot {slot}");
            p.on_submitted(early);
        }
        p.mark_dirty();
        assert!(!p.should_submit(t0 + 6 * 20 * MS - 3 * MS));
        assert!(p.should_submit(t0 + 6 * 20 * MS - 2 * MS));
    }

    #[test]
    fn late_frame_within_an_interval_keeps_the_grid() {
        let t0 = Instant::now();
        let mut p = FramePacer::new(50);
        p.mark_dirty();
        p.on_submitted(t0);
        p.mark_dirty();
        p.on_submitted(t0 + 30 * MS); // 10 ms late for the 20 ms slot
        p.mark_dirty();
        // Next slot is still 40 ms, so it is due again at once.
        assert!(p.should_submit(t0 + 38 * MS));
    }

    #[test]
    fn frame_later_than_an_interval_realigns_the_grid() {
        let t0 = Instant::now();
        let mut p = FramePacer::new(50);
        p.mark_dirty();
        p.on_submitted(t0);
        p.mark_dirty();
        p.on_submitted(t0 + 65 * MS); // more than one interval late
        p.mark_dirty();
        assert!(!p.should_submit(t0 + 65 * MS + 17 * MS));
        assert!(p.should_submit(t0 + 65 * MS + 18 * MS));
    }

    #[test]
    fn skipped_slot_keeps_image_pending_for_next_slot() {
        let t0 = Instant::now();
        let mut p = FramePacer::new(50);
        p.mark_dirty();
        p.on_skipped(t0);
        assert!(p.is_dirty());
        assert!(!p.should_submit(t0 + 17 * MS));
        assert_eq!(p.capture_timeout(t0 + 5 * MS), 13 * MS);
        assert!(p.should_submit(t0 + 18 * MS));
    }

    #[test]
    fn static_screen_idles() {
        let t0 = Instant::now();
        let mut p = FramePacer::new(50);
        p.mark_dirty();
        p.on_submitted(t0);
        let later = t0 + Duration::from_secs(10);
        assert!(!p.should_submit(later));
        assert_eq!(p.capture_timeout(later), 20 * MS);
        assert_eq!(p.capture_timeout(t0), 20 * MS);
    }

    #[test]
    fn change_after_idle_is_immediate_and_restarts_the_grid() {
        let t0 = Instant::now();
        let mut p = FramePacer::new(50);
        p.mark_dirty();
        p.on_submitted(t0);
        let later = t0 + Duration::from_secs(1);
        p.mark_dirty();
        assert!(p.should_submit(later));
        p.on_submitted(later);
        p.mark_dirty();
        assert!(!p.should_submit(later + 10 * MS));
        assert!(p.should_submit(later + 18 * MS));
    }
}
