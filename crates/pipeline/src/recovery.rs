//! The viewer's side of loss recovery: when to ask the host for missing
//! datagrams ([`Nack`]) and when to stop waiting for them.
//!
//! A frame missing datagrams is asked for as soon as the loss is evident:
//! a newer datagram arrived (for the newest frame, which may simply still
//! be arriving, after [`TAIL_GRACE`] without datagrams). The host answers
//! within about a round trip; frames after the incomplete one wait for it,
//! since frames are decoded in order. If the resend does not complete it
//! within [`recovery_wait`] the viewer gives the frame up and asks for a
//! keyframe, as it did before recovery existed.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use protocol::recovery::Nack;
use protocol::video::Gap;

use crate::burst::BURST_GAP;

/// Silence after which the newest frame's missing datagrams are asked for.
/// A large frame arrives in bursts [`BURST_GAP`] apart; the grace covers a
/// gap plus the timer's slack, so a frame still on its way is not asked
/// for again.
pub const TAIL_GRACE: Duration = Duration::from_millis(4);

const _: () = assert!(TAIL_GRACE.as_millis() >= 2 * BURST_GAP.as_millis());

/// Round-trip time assumed until the transport measured one: a LAN's.
pub const ASSUMED_RTT: Duration = Duration::from_millis(5);

/// Added to the round-trip time when waiting for a resend: the host's
/// scheduling and the viewer's own wake-up.
pub const RECOVERY_MARGIN: Duration = Duration::from_millis(10);

/// Longest wait for a resend, whatever the round-trip time: beyond it a
/// keyframe is faster than recovery, and the host may have forgotten the
/// frame (`crate::retransmit::RETRANSMIT_WINDOW`).
pub const MAX_RECOVERY_WAIT: Duration = Duration::from_millis(150);

/// How long to wait for the datagrams a [`Nack`] asked for: one and a half
/// round trips plus [`RECOVERY_MARGIN`], at most [`MAX_RECOVERY_WAIT`].
pub fn recovery_wait(rtt: Duration) -> Duration {
    (rtt.saturating_mul(3) / 2)
        .saturating_add(RECOVERY_MARGIN)
        .min(MAX_RECOVERY_WAIT)
}

/// What to do about the gaps at one moment.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RecoveryStep {
    /// Requests to send now.
    pub nacks: Vec<Nack>,
    /// Stop waiting for every frame up to this one: its resend is overdue.
    pub give_up_through: Option<u32>,
}

/// Tracks the frames asked for and their deadlines.
#[derive(Debug)]
pub struct LossRecovery {
    rtt: Duration,
    /// Frames asked for, with the moment to give up on them.
    deadlines: BTreeMap<u32, Instant>,
    /// When the newest frame's missing datagrams will be asked for, if it
    /// is incomplete and not asked for yet.
    tail_due: Option<Instant>,
}

impl LossRecovery {
    /// `rtt` is the round-trip time assumed until [`Self::set_rtt`].
    pub fn new(rtt: Duration) -> Self {
        Self {
            rtt,
            deadlines: BTreeMap::new(),
            tail_due: None,
        }
    }

    /// The network's current round-trip time, for the next deadlines.
    pub fn set_rtt(&mut self, rtt: Duration) {
        self.rtt = rtt;
    }

    /// Whether `frame_id` was asked for and is still awaited.
    pub fn is_awaited(&self, frame_id: u32) -> bool {
        self.deadlines.contains_key(&frame_id)
    }

    /// Decides, at `now`, about the reassembler's current `gaps` (oldest
    /// first); `last_datagram` is when the latest datagram arrived. Frames
    /// no longer among the gaps are forgotten.
    pub fn poll(&mut self, gaps: &[Gap], last_datagram: Instant, now: Instant) -> RecoveryStep {
        self.deadlines
            .retain(|frame_id, _| gaps.iter().any(|gap| gap.frame_id == *frame_id));
        self.tail_due = None;
        let mut step = RecoveryStep::default();
        for gap in gaps {
            if let Some(&deadline) = self.deadlines.get(&gap.frame_id) {
                if now >= deadline {
                    step.give_up_through = Some(gap.frame_id);
                }
                continue;
            }
            let ask_at = if gap.newest {
                last_datagram + TAIL_GRACE
            } else {
                now
            };
            if now < ask_at {
                self.tail_due = Some(ask_at);
                continue;
            }
            self.deadlines
                .insert(gap.frame_id, now + recovery_wait(self.rtt));
            step.nacks.push(Nack {
                frame_id: gap.frame_id,
                missing: gap.missing.clone(),
            });
        }
        step
    }

    /// The next moment [`Self::poll`] has something to do without new
    /// datagrams: a deadline, or the newest frame's grace running out.
    pub fn next_wakeup(&self) -> Option<Instant> {
        self.deadlines.values().copied().chain(self.tail_due).min()
    }
}

#[cfg(test)]
mod tests {
    use protocol::recovery::Missing;

    use super::*;

    const RTT: Duration = Duration::from_millis(2);

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    fn gap(frame_id: u32, newest: bool) -> Gap {
        Gap {
            frame_id,
            missing: Missing::Packets(vec![1]),
            newest,
        }
    }

    fn nack(frame_id: u32) -> Nack {
        Nack {
            frame_id,
            missing: Missing::Packets(vec![1]),
        }
    }

    #[test]
    fn the_wait_is_a_round_trip_and_a_half_plus_a_margin_capped() {
        assert_eq!(
            recovery_wait(Duration::from_millis(2)),
            Duration::from_millis(13)
        );
        assert_eq!(recovery_wait(Duration::from_secs(1)), MAX_RECOVERY_WAIT);
        assert_eq!(recovery_wait(Duration::MAX), MAX_RECOVERY_WAIT);
    }

    #[test]
    fn an_older_frame_is_asked_for_at_once_and_only_once() {
        let base = Instant::now();
        let mut recovery = LossRecovery::new(RTT);
        let gaps = [gap(3, false)];
        let step = recovery.poll(&gaps, base, base);
        assert_eq!(step.nacks, vec![nack(3)]);
        assert_eq!(step.give_up_through, None);
        assert!(recovery.is_awaited(3));
        assert_eq!(
            recovery.poll(&gaps, base, ms(base, 1)),
            RecoveryStep::default()
        );
    }

    #[test]
    fn the_newest_frame_gets_a_grace_for_its_last_datagrams() {
        let base = Instant::now();
        let mut recovery = LossRecovery::new(RTT);
        let gaps = [gap(3, true)];
        assert!(recovery.poll(&gaps, base, ms(base, 1)).nacks.is_empty());
        assert_eq!(recovery.next_wakeup(), Some(base + TAIL_GRACE));
        let step = recovery.poll(&gaps, base, base + TAIL_GRACE);
        assert_eq!(step.nacks, vec![nack(3)]);
    }

    #[test]
    fn an_overdue_resend_gives_up_through_the_newest_overdue_frame() {
        let base = Instant::now();
        let mut recovery = LossRecovery::new(RTT);
        let gaps = [gap(3, false), gap(4, false), gap(6, true)];
        recovery.poll(&gaps, base, base);
        let deadline = base + recovery_wait(RTT);
        assert_eq!(
            recovery.next_wakeup(),
            Some(deadline.min(base + TAIL_GRACE))
        );
        let before = deadline - Duration::from_millis(1);
        assert_eq!(recovery.poll(&gaps, before, before).give_up_through, None);
        let step = recovery.poll(&gaps, before, deadline);
        assert_eq!(step.give_up_through, Some(4));
    }

    #[test]
    fn frames_that_are_no_longer_gaps_are_forgotten() {
        let base = Instant::now();
        let mut recovery = LossRecovery::new(RTT);
        recovery.poll(&[gap(3, false)], base, base);
        assert_eq!(
            recovery.poll(&[], base, ms(base, 1)),
            RecoveryStep::default()
        );
        assert!(!recovery.is_awaited(3));
        assert_eq!(recovery.next_wakeup(), None);
    }

    #[test]
    fn the_deadline_follows_the_measured_round_trip() {
        let base = Instant::now();
        let mut recovery = LossRecovery::new(RTT);
        recovery.set_rtt(Duration::from_millis(40));
        recovery.poll(&[gap(1, false)], base, base);
        assert_eq!(
            recovery.next_wakeup(),
            Some(base + recovery_wait(Duration::from_millis(40)))
        );
    }
}
