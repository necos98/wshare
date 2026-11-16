//! Which keyframe requests the host acts on.
//!
//! A keyframe is the largest frame of the stream: sent in one go it is
//! the likeliest to lose datagrams, and every loss used to cost another
//! request, hence another keyframe. Requests that arrive while a keyframe
//! is on its way are therefore ignored: the viewer sent them before that
//! keyframe reached it, and should the keyframe itself lose datagrams, the
//! viewer recovers them with a [`protocol::recovery::Nack`]. If the viewer
//! still lacks a keyframe it asks again after
//! [`crate::viewer::KEYFRAME_REQUEST_INTERVAL`], which this gate lets
//! through.

use std::time::{Duration, Instant};

use crate::viewer::KEYFRAME_REQUEST_INTERVAL;

/// Minimum time between a keyframe (forced or not) and the next forced one.
/// Covers a keyframe's encoding and its trip to the viewer on a LAN.
pub const MIN_FORCED_KEYFRAME_INTERVAL: Duration = Duration::from_millis(200);

// A viewer that repeats its request must get through, or a keyframe lost
// beyond repair would never be replaced.
const _: () =
    assert!(MIN_FORCED_KEYFRAME_INTERVAL.as_millis() < KEYFRAME_REQUEST_INTERVAL.as_millis());

/// Merges keyframe requests (see the module documentation).
#[derive(Debug, Default)]
pub struct KeyframeGate {
    /// When the last keyframe was forced or sent, whichever is later.
    last: Option<Instant>,
}

impl KeyframeGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a request received at `now` should force a keyframe: not
    /// when one was forced or sent less than
    /// [`MIN_FORCED_KEYFRAME_INTERVAL`] ago. An accepted request counts as
    /// forcing one.
    pub fn accept(&mut self, now: Instant) -> bool {
        let accepted = self
            .last
            .is_none_or(|last| now.saturating_duration_since(last) >= MIN_FORCED_KEYFRAME_INTERVAL);
        if accepted {
            self.last = Some(now);
        }
        accepted
    }

    /// A keyframe left at `now` (forced or the encoder's own).
    pub fn on_keyframe_sent(&mut self, now: Instant) {
        self.note(now);
    }

    /// The host forced a keyframe at `now` on its own, without a request
    /// (it knows the viewer cannot decode on): requests that follow are
    /// merged into it like into a requested one.
    pub fn on_keyframe_forced(&mut self, now: Instant) {
        self.note(now);
    }

    fn note(&mut self, now: Instant) {
        self.last = Some(self.last.map_or(now, |last| last.max(now)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    #[test]
    fn requests_while_a_keyframe_is_on_its_way_are_merged() {
        let base = Instant::now();
        let mut gate = KeyframeGate::new();
        assert!(gate.accept(base));
        assert!(!gate.accept(ms(base, 1)));
        gate.on_keyframe_sent(ms(base, 10));
        assert!(!gate.accept(ms(base, 150)));
        // Counted from the keyframe sent, not from the request.
        assert!(!gate.accept(ms(base, 205)));
        assert!(gate.accept(ms(base, 210)));
    }

    #[test]
    fn a_keyframe_of_the_encoder_s_own_also_closes_the_gate() {
        let base = Instant::now();
        let mut gate = KeyframeGate::new();
        gate.on_keyframe_sent(base);
        assert!(!gate.accept(ms(base, 100)));
        assert!(gate.accept(ms(base, 200)));
    }

    #[test]
    fn a_keyframe_the_host_forced_on_its_own_also_closes_the_gate() {
        let base = Instant::now();
        let mut gate = KeyframeGate::new();
        gate.on_keyframe_forced(base);
        assert!(!gate.accept(ms(base, 100)));
        assert!(gate.accept(ms(base, 200)));
    }

    #[test]
    fn a_repeated_request_of_the_viewer_gets_through() {
        let base = Instant::now();
        let mut gate = KeyframeGate::new();
        assert!(gate.accept(base));
        gate.on_keyframe_sent(base);
        let retry = base + KEYFRAME_REQUEST_INTERVAL;
        assert!(gate.accept(retry));
    }

    #[test]
    fn time_going_backwards_keeps_the_gate_closed() {
        let base = Instant::now() + Duration::from_secs(1);
        let mut gate = KeyframeGate::new();
        assert!(gate.accept(base));
        assert!(!gate.accept(base - Duration::from_millis(500)));
        gate.on_keyframe_sent(base - Duration::from_millis(500));
        assert!(!gate.accept(ms(base, 199)));
    }
}
