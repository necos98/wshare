//! Hand-off of video datagrams from the network task to the render thread.
//!
//! The queue between them is bounded. When the render thread falls behind,
//! the network task waits and the datagrams stay in quinn's receive buffer,
//! which drops the oldest ones when full: fresh video beats complete video.
//! At each wake-up the render thread takes everything waiting
//! ([`next_batch`]), so it decodes a backlog in one go and shows only its
//! newest picture instead of replaying it.
//!
//! The host's sound shares the datagram channel: audio datagrams are taken
//! out here and go straight to the jitter buffer ([`AudioIn`]), never
//! behind the video in this queue.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use bytes::Bytes;
use net::VideoChannel;
use protocol::audio::is_audio;

use super::audio::AudioIn;

/// Datagrams the queue holds, about 1.4 MB of video. A frame at the
/// default 20 Mbit/s and 60 fps is ~30 datagrams, so a render thread that
/// keeps up never comes close; the bound only matters when it does not.
pub const QUEUE_DATAGRAMS: usize = 1024;

/// The two ends of the hand-off queue.
pub fn queue() -> (SyncSender<Bytes>, Receiver<Bytes>) {
    sync_channel(QUEUE_DATAGRAMS)
}

/// Asks the render thread to forget the stream it follows, because the
/// connection was re-established and the host starts a new one.
///
/// The control task raises it, after the old datagrams stopped arriving
/// and before the new ones are forwarded; the render thread clears the
/// queue, resets the pipeline and then marks it done, so no datagram of the
/// new stream is lost to the clearing and none of the old one survives it.
#[derive(Debug, Clone, Default)]
pub struct Restart(Arc<AtomicBool>);

/// The video the window gets: the datagrams to show and the signal that the
/// stream starts over.
pub struct VideoIn {
    pub datagrams: Receiver<Bytes>,
    pub restart: Restart,
}

/// How often [`Restart::wait_done`] looks whether the render thread is done.
const RESTART_POLL: Duration = Duration::from_millis(10);

impl Restart {
    pub fn request(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// The render thread finished the restart.
    pub fn complete(&self) {
        self.0.store(false, Ordering::Release);
    }

    /// Waits up to `limit` for the restart to be done; `false`: it was not
    /// (the render thread is gone or stuck).
    pub async fn wait_done(&self, limit: Duration) -> bool {
        tokio::time::timeout(limit, async {
            while self.is_requested() {
                tokio::time::sleep(RESTART_POLL).await;
            }
        })
        .await
        .is_ok()
    }
}

/// Throws away the datagrams waiting in the queue.
pub fn discard_waiting(datagrams: &Receiver<Bytes>) {
    while datagrams.try_recv().is_ok() {}
}

/// Moves datagrams from the network to the render thread (video) and to
/// `audio` (sound) until either side is gone. Must run on a multi-thread
/// runtime: a full queue is waited on in place, which a current-thread
/// runtime does not allow.
pub async fn forward(video: VideoChannel, out: SyncSender<Bytes>, audio: AudioIn) {
    while let Ok(datagram) = video.recv().await {
        if !route(datagram, &out, &audio) {
            break; // render thread is gone
        }
    }
}

/// Hands `datagram` to the sound or the video. `false`: the render thread
/// is gone.
fn route(datagram: Bytes, out: &SyncSender<Bytes>, audio: &AudioIn) -> bool {
    if is_audio(&datagram) {
        audio.push(&datagram);
        return true;
    }
    hand_over(out, datagram)
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

    #[test]
    fn discarding_empties_the_queue() {
        let (tx, rx) = queue();
        for n in 0..5 {
            tx.send(datagram(n)).unwrap();
        }
        discard_waiting(&rx);
        let mut batch = Vec::new();
        assert_eq!(
            next_batch(&rx, NO_WAIT, &mut batch),
            Err(RecvTimeoutError::Timeout)
        );
    }

    #[tokio::test]
    async fn a_restart_is_done_when_the_render_thread_says_so() {
        let restart = Restart::default();
        assert!(!restart.is_requested());
        assert!(restart.wait_done(NO_WAIT).await, "nothing asked: done");
        restart.request();
        assert!(restart.is_requested());
        assert!(!restart.wait_done(NO_WAIT).await, "nobody completed it");
        let renderer = restart.clone();
        let thread = std::thread::spawn(move || renderer.complete());
        assert!(restart.wait_done(Duration::from_secs(10)).await);
        thread.join().unwrap();
        assert!(!restart.is_requested());
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
    async fn sound_never_enters_the_video_queue() {
        let (tx, rx) = queue();
        let audio = AudioIn::new();
        let sound = protocol::audio::packetize(0, 0, &[1, 2], 100).remove(0);
        assert!(route(sound, &tx, &audio));
        assert!(route(datagram(1), &tx, &audio));
        let mut batch = Vec::new();
        next_batch(&rx, NO_WAIT, &mut batch).unwrap();
        assert_eq!(batch, [datagram(1)]);
        drop(rx);
        assert!(!route(datagram(2), &tx, &audio));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hand_over_reports_a_gone_render_thread() {
        let (tx, rx) = queue();
        drop(rx);
        assert!(!hand_over(&tx, datagram(1)));
    }
}
