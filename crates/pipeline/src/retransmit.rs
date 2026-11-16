//! The host's memory of the video it just sent, so datagrams the viewer
//! missed can be sent again ([`protocol::recovery::Nack`]).
//!
//! Only a short span of video is worth resending: past it the viewer has
//! stopped waiting and asked for a keyframe. The buffer therefore keeps the
//! frames of the last [`RETRANSMIT_WINDOW`], and never more than
//! [`MAX_RETRANSMIT_BYTES`]. Datagrams are `Bytes` slices of the frame the
//! transport also holds, so keeping them costs no copy.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use protocol::recovery::{MAX_NACK_PACKETS, Missing, Nack};

use crate::recovery::MAX_RECOVERY_WAIT;
use crate::traits::{DatagramSink, DatagramTooLarge};

/// How long sent frames are kept. Longer than the viewer ever waits for a
/// resend ([`MAX_RECOVERY_WAIT`]) plus the time its request travels.
pub const RETRANSMIT_WINDOW: Duration = Duration::from_millis(200);

// The viewer must give up before the host forgets, or its last requests
// would always go unanswered.
const _: () = assert!(MAX_RECOVERY_WAIT.as_millis() < RETRANSMIT_WINDOW.as_millis());

/// Upper bound on the bytes kept, whatever the bitrate: 200 ms of a
/// 160 Mbit/s stream. At the default 20 Mbit/s the window holds ~0.5 MB.
pub const MAX_RETRANSMIT_BYTES: usize = 4 * 1024 * 1024;

/// How many requests for the same frame are answered. One resend repairs
/// an isolated loss; a second covers a lost resend. More would only let a
/// misbehaving viewer multiply the host's traffic.
pub const MAX_RESENDS_PER_FRAME: u8 = 2;

struct SentFrame {
    frame_id: u32,
    sent_at: Instant,
    datagrams: Vec<Bytes>,
    bytes: usize,
    resends: u8,
}

/// Frames sent in the last [`RETRANSMIT_WINDOW`], oldest first.
#[derive(Default)]
pub struct RetransmitBuffer {
    frames: VecDeque<SentFrame>,
    bytes: usize,
}

impl RetransmitBuffer {
    /// Remembers the datagrams of frame `frame_id`, sent at `now`. Frame ids
    /// must increase from call to call. Frames older than the window are
    /// forgotten, then the oldest ones while over [`MAX_RETRANSMIT_BYTES`]
    /// (the newest frame is always kept).
    pub fn record(&mut self, frame_id: u32, datagrams: &[Bytes], now: Instant) {
        self.expire(now);
        let bytes = datagrams.iter().map(Bytes::len).sum();
        self.frames.push_back(SentFrame {
            frame_id,
            sent_at: now,
            datagrams: datagrams.to_vec(),
            bytes,
            resends: 0,
        });
        self.bytes += bytes;
        while self.bytes > MAX_RETRANSMIT_BYTES && self.frames.len() > 1 {
            self.pop_oldest();
        }
    }

    /// The datagrams `nack` asks for that are still kept, in the order
    /// asked (packet order for a whole frame), each at most once: never
    /// more than the frame has, however the list repeats itself.
    /// Empty when the frame is gone or was resent
    /// [`MAX_RESENDS_PER_FRAME`] times already.
    pub fn take_resend(&mut self, nack: &Nack, now: Instant) -> Vec<Bytes> {
        self.expire(now);
        let Ok(position) = self
            .frames
            .binary_search_by_key(&nack.frame_id, |frame| frame.frame_id)
        else {
            return Vec::new();
        };
        let frame = &mut self.frames[position];
        if frame.resends >= MAX_RESENDS_PER_FRAME {
            return Vec::new();
        }
        frame.resends += 1;
        match &nack.missing {
            Missing::Whole => frame.datagrams.clone(),
            // Lists longer than the protocol allows, or repeating an
            // index, come only from a misbehaving peer: their excess is not
            // looked at, and repeats are not sent again.
            Missing::Packets(indices) => {
                let mut asked = vec![false; frame.datagrams.len()];
                indices
                    .iter()
                    .take(MAX_NACK_PACKETS)
                    .filter_map(|&index| {
                        let index = usize::from(index);
                        let first_ask = !std::mem::replace(asked.get_mut(index)?, true);
                        first_ask.then(|| frame.datagrams[index].clone())
                    })
                    .collect()
            }
        }
    }

    /// Bytes currently kept.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    fn expire(&mut self, now: Instant) {
        while self
            .frames
            .front()
            .is_some_and(|frame| now.saturating_duration_since(frame.sent_at) > RETRANSMIT_WINDOW)
        {
            self.pop_oldest();
        }
    }

    fn pop_oldest(&mut self) {
        if let Some(frame) = self.frames.pop_front() {
            self.bytes -= frame.bytes;
        }
    }
}

/// A [`RetransmitBuffer`] shared between the thread that sends the video
/// and the task that reads the viewer's requests, so a request is answered
/// at once instead of waiting for the next frame. Cheap to clone.
#[derive(Clone, Default)]
pub struct RecentFrames(Arc<Shared>);

#[derive(Default)]
struct Shared {
    buffer: Mutex<RetransmitBuffer>,
    /// Resent datagrams the transport refused as too large.
    too_large: AtomicU64,
}

impl RecentFrames {
    pub fn new() -> Self {
        Self::default()
    }

    /// See [`RetransmitBuffer::record`].
    pub fn record(&self, frame_id: u32, datagrams: &[Bytes], now: Instant) {
        self.lock().record(frame_id, datagrams, now);
    }

    /// Sends again, through `sink`, the datagrams `nack` asks for that are
    /// still kept. Returns how many were sent.
    ///
    /// A datagram over the transport's current limit ([`DatagramTooLarge`])
    /// is dropped and counted in [`Self::resends_too_large`]: it was sized
    /// for the limit of its first send, which QUIC lowers precisely after
    /// the losses that bring requests. Only other send errors, meaning the
    /// connection can no longer send, fail the resend.
    pub fn resend(&self, nack: &Nack, sink: &impl DatagramSink, now: Instant) -> Result<usize> {
        // Taken out first: the lock is not held while sending.
        let datagrams = self.lock().take_resend(nack, now);
        let mut too_large: usize = 0;
        for datagram in &datagrams {
            match sink.send(datagram.clone()) {
                Ok(()) => {}
                Err(error) if error.is::<DatagramTooLarge>() => too_large += 1,
                Err(error) => return Err(error),
            }
        }
        self.0
            .too_large
            .fetch_add(too_large as u64, Ordering::Relaxed);
        Ok(datagrams.len() - too_large)
    }

    /// Resent datagrams dropped so far because they no longer fit the
    /// transport's datagram limit.
    pub fn resends_too_large(&self) -> u64 {
        self.0.too_large.load(Ordering::Relaxed)
    }

    /// A poisoned lock only means a sender panicked mid-update; the buffer
    /// is still consistent enough to answer (at worst, nothing is resent).
    fn lock(&self) -> std::sync::MutexGuard<'_, RetransmitBuffer> {
        self.0.buffer.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn datagrams(frame_id: u32, count: u8, size: usize) -> Vec<Bytes> {
        (0..count)
            .map(|index| {
                let mut bytes = vec![index; size];
                bytes[0] = frame_id as u8;
                Bytes::from(bytes)
            })
            .collect()
    }

    fn nack(frame_id: u32, missing: Missing) -> Nack {
        Nack { frame_id, missing }
    }

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    #[test]
    fn listed_packets_are_resent_and_unknown_indices_ignored() {
        let base = Instant::now();
        let mut buffer = RetransmitBuffer::default();
        let sent = datagrams(4, 5, 10);
        buffer.record(4, &sent, base);
        let resent = buffer.take_resend(&nack(4, Missing::Packets(vec![1, 3, 9])), base);
        assert_eq!(resent, vec![sent[1].clone(), sent[3].clone()]);
        assert_eq!(resent[0].as_ptr(), sent[1].as_ptr(), "no copy");
    }

    #[test]
    fn an_oversized_list_is_only_read_up_to_the_protocol_limit() {
        let base = Instant::now();
        let mut buffer = RetransmitBuffer::default();
        let sent = datagrams(1, 2, 10);
        buffer.record(1, &sent, base);
        let mut indices = vec![0; MAX_NACK_PACKETS];
        indices.push(1);
        let resent = buffer.take_resend(&nack(1, Missing::Packets(indices)), base);
        assert_eq!(resent, vec![sent[0].clone()], "index 1 is past the limit");
    }

    #[test]
    fn a_repeated_index_is_resent_once() {
        let base = Instant::now();
        let mut buffer = RetransmitBuffer::default();
        let sent = datagrams(1, 3, 10);
        buffer.record(1, &sent, base);
        let flood = buffer.take_resend(&nack(1, Missing::Packets(vec![0; MAX_NACK_PACKETS])), base);
        assert_eq!(flood, vec![sent[0].clone()]);
        let mixed = buffer.take_resend(&nack(1, Missing::Packets(vec![2, 0, 2, 1, 0])), base);
        assert_eq!(
            mixed,
            vec![sent[2].clone(), sent[0].clone(), sent[1].clone()]
        );
    }

    #[test]
    fn a_whole_frame_request_resends_every_packet() {
        let base = Instant::now();
        let mut buffer = RetransmitBuffer::default();
        buffer.record(1, &datagrams(1, 2, 10), base);
        buffer.record(2, &datagrams(2, 3, 10), base);
        assert_eq!(buffer.take_resend(&nack(2, Missing::Whole), base).len(), 3);
    }

    #[test]
    fn unknown_frames_get_nothing() {
        let base = Instant::now();
        let mut buffer = RetransmitBuffer::default();
        buffer.record(5, &datagrams(5, 2, 10), base);
        assert!(
            buffer
                .take_resend(&nack(4, Missing::Whole), base)
                .is_empty()
        );
        assert!(
            buffer
                .take_resend(&nack(6, Missing::Whole), base)
                .is_empty()
        );
    }

    #[test]
    fn frames_older_than_the_window_are_forgotten() {
        let base = Instant::now();
        let window = RETRANSMIT_WINDOW.as_millis() as u64;
        let mut buffer = RetransmitBuffer::default();
        buffer.record(1, &datagrams(1, 2, 10), base);
        buffer.record(2, &datagrams(2, 2, 10), ms(base, 50));
        assert_eq!(buffer.bytes(), 40);
        // At the window's edge frame 1 is still there...
        let at_edge = ms(base, window);
        assert_eq!(
            buffer.take_resend(&nack(1, Missing::Whole), at_edge).len(),
            2
        );
        // ...just past it, gone, and its bytes with it.
        let past = ms(base, window + 1);
        assert!(
            buffer
                .take_resend(&nack(1, Missing::Whole), past)
                .is_empty()
        );
        assert_eq!(buffer.bytes(), 20);
        assert_eq!(buffer.take_resend(&nack(2, Missing::Whole), past).len(), 2);
    }

    #[test]
    fn recording_drops_expired_frames_even_without_requests() {
        let base = Instant::now();
        let mut buffer = RetransmitBuffer::default();
        buffer.record(1, &datagrams(1, 2, 10), base);
        buffer.record(2, &datagrams(2, 2, 10), base + RETRANSMIT_WINDOW * 2);
        assert_eq!(buffer.bytes(), 20);
    }

    #[test]
    fn the_byte_budget_drops_the_oldest_frames_but_keeps_the_newest() {
        let base = Instant::now();
        let mut buffer = RetransmitBuffer::default();
        let size = MAX_RETRANSMIT_BYTES / 4;
        for frame_id in 0..5 {
            buffer.record(frame_id, &datagrams(frame_id, 1, size), base);
        }
        assert_eq!(buffer.bytes(), 4 * size);
        assert!(
            buffer
                .take_resend(&nack(0, Missing::Whole), base)
                .is_empty()
        );
        assert_eq!(buffer.take_resend(&nack(1, Missing::Whole), base).len(), 1);
        // A frame larger than the budget on its own is still kept.
        buffer.record(9, &datagrams(9, 1, MAX_RETRANSMIT_BYTES + 1), base);
        assert_eq!(buffer.bytes(), MAX_RETRANSMIT_BYTES + 1);
        assert_eq!(buffer.take_resend(&nack(9, Missing::Whole), base).len(), 1);
    }

    #[test]
    fn a_frame_is_resent_a_bounded_number_of_times() {
        let base = Instant::now();
        let mut buffer = RetransmitBuffer::default();
        buffer.record(1, &datagrams(1, 2, 10), base);
        for _ in 0..MAX_RESENDS_PER_FRAME {
            assert_eq!(buffer.take_resend(&nack(1, Missing::Whole), base).len(), 2);
        }
        assert!(
            buffer
                .take_resend(&nack(1, Missing::Whole), base)
                .is_empty()
        );
    }

    /// Records what it is given; refuses datagrams over `limit` like a
    /// transport whose limit shrank, and everything once `broken`.
    struct RecordingSink {
        sent: RefCell<Vec<Bytes>>,
        limit: usize,
        broken: bool,
    }

    impl RecordingSink {
        fn with_limit(limit: usize) -> Self {
            Self {
                sent: RefCell::new(Vec::new()),
                limit,
                broken: false,
            }
        }
    }

    impl DatagramSink for RecordingSink {
        fn max_datagram_size(&self) -> usize {
            self.limit
        }

        fn send(&self, datagram: Bytes) -> Result<()> {
            if self.broken {
                anyhow::bail!("connection lost");
            }
            if datagram.len() > self.limit {
                return Err(DatagramTooLarge {
                    len: datagram.len(),
                    limit: self.limit,
                }
                .into());
            }
            self.sent.borrow_mut().push(datagram);
            Ok(())
        }

        fn backlog_bytes(&self) -> usize {
            0
        }
    }

    #[test]
    fn shared_frames_are_resent_through_the_sink() {
        let base = Instant::now();
        let recent = RecentFrames::new();
        let sent = datagrams(3, 3, 10);
        recent.clone().record(3, &sent, base);
        let sink = RecordingSink::with_limit(1200);
        let count = recent
            .resend(&nack(3, Missing::Packets(vec![2])), &sink, base)
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(*sink.sent.borrow(), vec![sent[2].clone()]);
        assert_eq!(recent.resends_too_large(), 0);
    }

    #[test]
    fn resends_over_a_shrunk_limit_are_dropped_and_counted() {
        let base = Instant::now();
        let recent = RecentFrames::new();
        let small = datagrams(1, 1, 10).remove(0);
        let large = datagrams(1, 1, 30).remove(0);
        let sent = vec![small.clone(), large, small.clone()];
        recent.record(1, &sent, base);
        let sink = RecordingSink::with_limit(20);
        let count = recent
            .resend(&nack(1, Missing::Whole), &sink, base)
            .unwrap();
        assert_eq!(count, 2, "the others still go");
        assert_eq!(*sink.sent.borrow(), vec![small.clone(), small]);
        assert_eq!(recent.resends_too_large(), 1);
    }

    #[test]
    fn other_send_errors_still_fail_the_resend() {
        let base = Instant::now();
        let recent = RecentFrames::new();
        recent.record(1, &datagrams(1, 2, 10), base);
        let sink = RecordingSink {
            broken: true,
            ..RecordingSink::with_limit(1200)
        };
        let error = recent
            .resend(&nack(1, Missing::Whole), &sink, base)
            .unwrap_err();
        assert!(!error.is::<DatagramTooLarge>());
        assert_eq!(recent.resends_too_large(), 0);
    }
}
