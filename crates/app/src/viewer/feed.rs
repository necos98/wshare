//! Hand-off of video datagrams from the network task to the render thread.
//!
//! The queue between them is bounded. When the render thread falls behind,
//! the network task waits and the datagrams stay in quinn's receive buffer,
//! which drops the oldest ones when full: fresh video beats complete video.
//! At each wake-up the render thread takes everything waiting
//! ([`next_batch`]), so it decodes a backlog in one go and shows only its
//! newest picture instead of replaying it.

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use bytes::Bytes;
use net::VideoChannel;

/// Datagrams the queue holds, about 1.4 MB of video. A frame at the
/// default 20 Mbit/s and 60 fps is ~30 datagrams, so a render thread that
/// keeps up never comes close; the bound only matters when it does not.
pub const QUEUE_DATAGRAMS: usize = 1024;

/// The two ends of the hand-off queue.
pub fn queue() -> (SyncSender<Bytes>, Receiver<Bytes>) {
    sync_channel(QUEUE_DATAGRAMS)
}

/// Moves datagrams from the network to the render thread until either side
/// is gone. Must run on a multi-thread runtime: a full queue is waited on
/// in place, which a current-thread runtime does not allow.
pub async fn forward(video: VideoChannel, out: SyncSender<Bytes>) {
    while let Ok(datagram) = video.recv().await {
        if !hand_over(&out, datagram) {
            break; // render thread is gone
        }
    }
}

/// Queues `datagram`, waiting while the queue is full. `false`: the render
/// thread is gone.
fn hand_over(out: &SyncSender<Bytes>, datagram: Bytes) -> bool {
    match out.try_send(datagram) {
        Ok(()) => true,
        Err(TrySendError::Full(datagram)) => {
            tokio::task::block_in_place(|| out.send(datagram).is_ok())
        }
        Err(TrySendError::Disconnected(_)) => false,
    }
}

/// Waits up to `timeout` for a datagram, then takes every other one already
/// waiting, at most [`QUEUE_DATAGRAMS`] in all so a sender that never stops
/// cannot hold the loop. `batch` is cleared first and reused to avoid an
/// allocation per wake-up.
pub fn next_batch(
    datagrams: &Receiver<Bytes>,
    timeout: Duration,
    batch: &mut Vec<Bytes>,
) -> Result<(), RecvTimeoutError> {
    batch.clear();
    batch.push(datagrams.recv_timeout(timeout)?);
    while batch.len() < QUEUE_DATAGRAMS {
        match datagrams.try_recv() {
            Ok(datagram) => batch.push(datagram),
            // Empty: done for now. Disconnected: the next call reports it.
            Err(_) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_WAIT: Duration = Duration::from_millis(1);

    fn datagram(n: u8) -> Bytes {
        Bytes::from(vec![n])
    }

    #[test]
    fn batch_takes_everything_waiting_in_order() {
        let (tx, rx) = queue();
        for n in 1..=3 {
            tx.send(datagram(n)).unwrap();
        }
        let mut batch = vec![datagram(99)];
        next_batch(&rx, NO_WAIT, &mut batch).unwrap();
        assert_eq!(batch, [datagram(1), datagram(2), datagram(3)]);
    }

    #[test]
    fn empty_queue_times_out_and_closed_queue_disconnects() {
        let (tx, rx) = queue();
        let mut batch = Vec::new();
        assert_eq!(
            next_batch(&rx, NO_WAIT, &mut batch),
            Err(RecvTimeoutError::Timeout)
        );
        tx.send(datagram(1)).unwrap();
        drop(tx);
        next_batch(&rx, NO_WAIT, &mut batch).unwrap();
        assert_eq!(batch, [datagram(1)]);
        assert_eq!(
            next_batch(&rx, NO_WAIT, &mut batch),
            Err(RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn queue_is_bounded_and_a_batch_takes_it_whole() {
        let (tx, rx) = queue();
        for n in 0..QUEUE_DATAGRAMS {
            tx.try_send(datagram(n as u8)).unwrap();
        }
        assert!(matches!(
            tx.try_send(datagram(0)),
            Err(TrySendError::Full(_))
        ));
        let mut batch = Vec::new();
        next_batch(&rx, NO_WAIT, &mut batch).unwrap();
        assert_eq!(batch.len(), QUEUE_DATAGRAMS);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn full_queue_waits_for_the_render_thread() {
        let (tx, rx) = queue();
        for n in 0..QUEUE_DATAGRAMS {
            assert!(hand_over(&tx, datagram(n as u8)));
        }
        let reader = std::thread::spawn(move || {
            let mut batch = Vec::new();
            next_batch(&rx, Duration::from_secs(10), &mut batch).unwrap();
            next_batch(&rx, Duration::from_secs(10), &mut batch).unwrap();
            batch
        });
        // Blocks until the reader made room, then succeeds.
        assert!(hand_over(&tx, datagram(7)));
        drop(tx);
        assert_eq!(reader.join().unwrap(), [datagram(7)]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hand_over_reports_a_gone_render_thread() {
        let (tx, rx) = queue();
        drop(rx);
        assert!(!hand_over(&tx, datagram(1)));
    }
}
