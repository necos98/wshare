//! What the render loop does with the stream's requests: the control
//! messages to send, and how long it may sleep before the stream has
//! recovery work due.

use std::time::{Duration, Instant};

use pipeline::ViewerAction;
use protocol::control::ControlMessage;

/// How long to wait for datagrams: one `tick`, or less when the stream has
/// recovery work due earlier (`wakeup`).
pub fn wait_for(tick: Duration, wakeup: Option<Instant>, now: Instant) -> Duration {
    wakeup.map_or(tick, |at| at.saturating_duration_since(now).min(tick))
}

/// The control messages for the stream's `actions`, plus a keyframe
/// request when the silence calls for a `probe` (one request at most).
pub fn requests(actions: Vec<ViewerAction>, probe: bool) -> Vec<ControlMessage> {
    let mut keyframe = probe;
    let mut messages: Vec<ControlMessage> = actions
        .into_iter()
        .filter_map(|action| match action {
            ViewerAction::RequestKeyframe => {
                keyframe = true;
                None
            }
            ViewerAction::Nack(nack) => Some(ControlMessage::Nack(nack)),
        })
        .collect();
    if keyframe {
        messages.push(ControlMessage::RequestKeyframe);
    }
    messages
}

#[cfg(test)]
mod tests {
    use protocol::recovery::{Missing, Nack};

    use super::*;

    const TICK: Duration = Duration::from_millis(50);

    #[test]
    fn the_wait_is_cut_short_by_recovery_work() {
        let now = Instant::now();
        assert_eq!(wait_for(TICK, None, now), TICK);
        let soon = now + Duration::from_millis(3);
        assert_eq!(wait_for(TICK, Some(soon), now), Duration::from_millis(3));
        assert_eq!(wait_for(TICK, Some(now + TICK * 2), now), TICK);
        // Already due: no wait at all.
        assert_eq!(wait_for(TICK, Some(now), soon), Duration::ZERO);
    }

    #[test]
    fn actions_become_messages_with_at_most_one_keyframe_request() {
        let nack = Nack {
            frame_id: 4,
            missing: Missing::Whole,
        };
        assert_eq!(
            requests(
                vec![
                    ViewerAction::Nack(nack.clone()),
                    ViewerAction::RequestKeyframe
                ],
                true
            ),
            vec![ControlMessage::Nack(nack), ControlMessage::RequestKeyframe]
        );
        assert_eq!(
            requests(Vec::new(), true),
            vec![ControlMessage::RequestKeyframe]
        );
        assert!(requests(Vec::new(), false).is_empty());
    }
}
