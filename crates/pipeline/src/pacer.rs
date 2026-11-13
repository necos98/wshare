//! Decides when a captured frame should be encoded.
//!
//! Rules:
//! - Only encode when the screen changed ("dirty"); a static screen costs
//!   nothing.
//! - Never encode more often than the target fps: if the screen changes
//!   faster, intermediate images are skipped and the latest one is sent at
//!   the next slot (the latest image is never lost, only delayed to its slot).
//! - The very first frame may be sent immediately.
//!
//! Time is passed in explicitly so the logic is deterministic in tests.

use std::time::{Duration, Instant};

pub struct FramePacer {
    interval: Duration,
    dirty: bool,
    last_submit: Option<Instant>,
}

impl FramePacer {
    /// Panics if `fps` is 0.
    pub fn new(fps: u32) -> Self {
        assert!(fps > 0, "fps must be positive");
        Self {
            interval: Duration::from_secs(1) / fps,
            dirty: false,
            last_submit: None,
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
    /// this is the time left until the next slot (zero if already due);
    /// otherwise one interval.
    pub fn capture_timeout(&self, now: Instant) -> Duration {
        if !self.dirty {
            return self.interval;
        }
        self.next_slot()
            .map_or(Duration::ZERO, |slot| slot.saturating_duration_since(now))
    }

    /// True when dirty and the next slot has been reached.
    pub fn should_submit(&self, now: Instant) -> bool {
        self.dirty && self.next_slot().is_none_or(|slot| now >= slot)
    }

    /// Records that a frame was submitted at `now`; clears the dirty flag.
    pub fn on_submitted(&mut self, now: Instant) {
        self.last_submit = Some(now);
        self.dirty = false;
    }

    /// Earliest time the next frame may be submitted (`None`: right away).
    fn next_slot(&self) -> Option<Instant> {
        self.last_submit.map(|last| last + self.interval)
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
    fn rate_is_capped() {
        let t0 = Instant::now();
        let mut p = FramePacer::new(50);
        p.mark_dirty();
        p.on_submitted(t0);
        assert!(!p.is_dirty());
        p.mark_dirty();
        assert!(!p.should_submit(t0 + 5 * MS));
        assert_eq!(p.capture_timeout(t0 + 5 * MS), 15 * MS);
        assert!(!p.should_submit(t0 + 19 * MS));
        assert!(p.should_submit(t0 + 20 * MS));
        assert_eq!(p.capture_timeout(t0 + 25 * MS), Duration::ZERO);
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
        assert!(!p.should_submit(t0 + 40 * MS));
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
    fn change_after_idle_is_immediate() {
        let t0 = Instant::now();
        let mut p = FramePacer::new(50);
        p.mark_dirty();
        p.on_submitted(t0);
        p.mark_dirty();
        assert!(p.should_submit(t0 + Duration::from_secs(1)));
    }
}
