//! Batching of the viewer's outgoing control messages.
//!
//! The mouse can report hundreds of positions a second, each of which would
//! be awaited on the control stream one by one. Positions are absolute, so
//! when several wait in the queue only the last one matters: the older ones
//! are dropped. Everything else (clicks, keys, wheel, requests) is sent
//! as it is, in the order it was produced.

use protocol::control::ControlMessage;
use protocol::input::InputEvent;
use tokio::sync::mpsc::UnboundedReceiver;

/// Most messages taken from the queue at once, so a flood of input cannot
/// keep the control loop from looking at what the host sent.
const MAX_BATCH: usize = 256;

/// `first` plus whatever else is already queued, with runs of mouse moves
/// merged (see [`coalesce_moves`]). Never waits.
pub fn next_batch(
    first: ControlMessage,
    queue: &mut UnboundedReceiver<ControlMessage>,
) -> Vec<ControlMessage> {
    let mut batch = vec![first];
    while batch.len() < MAX_BATCH {
        match queue.try_recv() {
            Ok(message) => batch.push(message),
            Err(_) => break,
        }
    }
    coalesce_moves(batch)
}

/// Replaces every run of consecutive absolute mouse moves by its last one.
/// Moves separated by any other message are kept apart: a click must land
/// where the pointer was when it happened.
pub fn coalesce_moves(messages: Vec<ControlMessage>) -> Vec<ControlMessage> {
    let mut merged: Vec<ControlMessage> = Vec::with_capacity(messages.len());
    for message in messages {
        let supersedes_last = is_move(&message) && merged.last().is_some_and(is_move);
        if supersedes_last {
            merged.pop();
        }
        merged.push(message);
    }
    merged
}

fn is_move(message: &ControlMessage) -> bool {
    matches!(message, ControlMessage::Input(InputEvent::MouseMove { .. }))
}

#[cfg(test)]
mod tests {
    use protocol::input::MouseButton;
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    fn at(x: f32) -> ControlMessage {
        ControlMessage::Input(InputEvent::MouseMove { x, y: 0.5 })
    }

    fn click(pressed: bool) -> ControlMessage {
        ControlMessage::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed,
        })
    }

    fn key(scancode: u16) -> ControlMessage {
        ControlMessage::Input(InputEvent::Key {
            scancode,
            extended: false,
            pressed: true,
        })
    }

    #[test]
    fn a_run_of_moves_keeps_only_the_last() {
        assert_eq!(
            coalesce_moves(vec![at(0.1), at(0.2), at(0.3)]),
            vec![at(0.3)]
        );
    }

    #[test]
    fn clicks_and_keys_are_never_merged_or_reordered() {
        let sequence = vec![
            click(true),
            click(false),
            key(30),
            key(30),
            ControlMessage::Input(InputEvent::ReleaseAll),
            ControlMessage::RequestKeyframe,
        ];
        assert_eq!(coalesce_moves(sequence.clone()), sequence);
    }

    #[test]
    fn moves_around_a_click_stay_on_each_side_of_it() {
        let batch = vec![
            at(0.1),
            at(0.2),
            click(true),
            at(0.3),
            at(0.4),
            click(false),
        ];
        assert_eq!(
            coalesce_moves(batch),
            vec![at(0.2), click(true), at(0.4), click(false)]
        );
    }

    #[test]
    fn empty_and_single_batches_are_unchanged() {
        assert!(coalesce_moves(Vec::new()).is_empty());
        assert_eq!(coalesce_moves(vec![at(0.1)]), vec![at(0.1)]);
    }

    #[test]
    fn next_batch_takes_what_is_queued_and_merges_it() {
        let (tx, mut rx) = unbounded_channel();
        for message in [at(0.2), at(0.3), click(true), at(0.4)] {
            tx.send(message).unwrap();
        }
        assert_eq!(
            next_batch(at(0.1), &mut rx),
            vec![at(0.3), click(true), at(0.4)]
        );
        assert!(rx.try_recv().is_err(), "the queue was drained");
    }

    #[test]
    fn next_batch_is_bounded() {
        let (tx, mut rx) = unbounded_channel();
        for scancode in 0..(MAX_BATCH as u16 * 2) {
            tx.send(key(scancode)).unwrap();
        }
        assert_eq!(next_batch(key(999), &mut rx).len(), MAX_BATCH);
        assert!(rx.try_recv().is_ok(), "the rest waits for the next round");
    }
}
