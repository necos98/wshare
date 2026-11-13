//! Host loop: capture -> encode -> packetize -> send.
//!
//! Runs on a dedicated (blocking) thread; the app calls [`HostStreamer::step`]
//! in a loop. Keyframe requests arriving from the network are forwarded with
//! [`HostStreamer::request_keyframe`].

use std::collections::VecDeque;
use std::time::Instant;

use anyhow::Result;
use protocol::video::Packetizer;

use crate::pacer::FramePacer;
use crate::traits::{DatagramSink, EncodedFrame, FrameSource, VideoEncoder};

/// Upper bound on encoder events awaited after a submit (an async encoder
/// typically emits "need input" then "have output").
const MAX_FLUSH_EVENTS: usize = 8;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostStats {
    pub frames_submitted: u64,
    pub frames_sent: u64,
    pub keyframes_sent: u64,
    /// Bytes handed to the sink, datagram headers included.
    pub bytes_sent: u64,
    pub datagrams_sent: u64,
}

pub struct HostStreamer<S, E, D>
where
    S: FrameSource,
    E: VideoEncoder<Frame = S::Frame>,
    D: DatagramSink,
{
    source: S,
    encoder: E,
    sink: D,
    pacer: FramePacer,
    packetizer: Packetizer,
    start: Instant,
    encoded: VecDeque<EncodedFrame>,
    stats: HostStats,
}

impl<S, E, D> HostStreamer<S, E, D>
where
    S: FrameSource,
    E: VideoEncoder<Frame = S::Frame>,
    D: DatagramSink,
{
    /// The pacer starts dirty so the first `step` sends an initial picture
    /// without waiting for the screen to change. Panics if `fps` is 0.
    pub fn new(source: S, encoder: E, sink: D, fps: u32) -> Self {
        let mut pacer = FramePacer::new(fps);
        pacer.mark_dirty();
        Self {
            source,
            encoder,
            sink,
            pacer,
            packetizer: Packetizer::new(),
            start: Instant::now(),
            encoded: VecDeque::new(),
            stats: HostStats::default(),
        }
    }

    /// One loop iteration:
    /// 1. poll the encoder (blocking only if it cannot accept input) and
    ///    send every finished frame, packetized to `sink.max_datagram_size()`
    ///    (re-read for every frame: the path MTU can change);
    /// 2. if not due yet, wait for a screen change up to the pacer timeout;
    /// 3. if the pacer says so and the encoder can accept, submit the
    ///    current frame with a timestamp relative to `start`.
    ///
    /// No busy loop: when nothing is due, step 2 blocks for up to one
    /// interval (static screen) or until the next slot (dirty screen); when a
    /// frame is due but the encoder is busy, step 1 of the next call blocks.
    pub fn step(&mut self) -> Result<()> {
        self.send_encoded_frames()?;
        self.wait_for_change_if_not_due()?;
        self.submit_if_due()
    }

    /// Forces the next encoded frame to be a keyframe AND marks the pacer
    /// dirty, so a keyframe is produced even if the screen is static.
    pub fn request_keyframe(&mut self) -> Result<()> {
        self.encoder.request_keyframe()?;
        self.pacer.mark_dirty();
        Ok(())
    }

    pub fn stats(&self) -> &HostStats {
        &self.stats
    }

    /// The frame source, for source-specific side channels between
    /// [`Self::step`]s (e.g. polling the mouse pointer shape a desktop
    /// capture gathers while waiting for frames). Driving the source's
    /// [`FrameSource`] methods directly would desynchronize the pacer.
    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }

    fn send_encoded_frames(&mut self) -> Result<()> {
        let block = !self.encoder.can_accept();
        self.encoder.poll(block, &mut self.encoded)?;
        while let Some(frame) = self.encoded.pop_front() {
            self.send_frame(&frame)?;
        }
        Ok(())
    }

    fn send_frame(&mut self, frame: &EncodedFrame) -> Result<()> {
        let max_size = self.sink.max_datagram_size();
        let datagrams =
            self.packetizer
                .packetize(&frame.data, frame.keyframe, frame.timestamp, max_size)?;
        for datagram in datagrams {
            let len = datagram.len() as u64;
            self.sink.send(datagram)?;
            self.stats.datagrams_sent += 1;
            self.stats.bytes_sent += len;
        }
        self.stats.frames_sent += 1;
        if frame.keyframe {
            self.stats.keyframes_sent += 1;
        }
        Ok(())
    }

    fn wait_for_change_if_not_due(&mut self) -> Result<()> {
        let now = Instant::now();
        if self.pacer.should_submit(now) {
            return Ok(());
        }
        // Not due implies a non-zero timeout: either not dirty (one interval)
        // or dirty with the next slot strictly in the future.
        if self
            .source
            .wait_for_change(self.pacer.capture_timeout(now))?
        {
            self.pacer.mark_dirty();
        }
        Ok(())
    }

    fn submit_if_due(&mut self) -> Result<()> {
        let now = Instant::now();
        if !self.pacer.should_submit(now) || !self.encoder.can_accept() {
            return Ok(());
        }
        let frame = self.source.current_frame()?;
        self.encoder.submit(&frame, self.timestamp(now))?;
        self.pacer.on_submitted(now);
        self.stats.frames_submitted += 1;
        self.flush_in_flight()
    }

    /// Waits for the frame just submitted and sends it right away. Without
    /// this, a frame finishing while we block in `wait_for_change` would sit
    /// in the encoder for up to one interval. Bounded by `MAX_FLUSH_EVENTS`
    /// so an encoder that buffers frames internally cannot stall the loop.
    fn flush_in_flight(&mut self) -> Result<()> {
        for _ in 0..MAX_FLUSH_EVENTS {
            if self.stats.frames_sent >= self.stats.frames_submitted {
                break;
            }
            self.encoder.poll(true, &mut self.encoded)?;
            while let Some(frame) = self.encoded.pop_front() {
                self.send_frame(&frame)?;
            }
        }
        Ok(())
    }

    /// Time since the stream started, in 100 ns units.
    fn timestamp(&self, now: Instant) -> i64 {
        let ticks = now.saturating_duration_since(self.start).as_nanos() / 100;
        i64::try_from(ticks).unwrap_or(i64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use protocol::video::{HEADER_LEN, ReassembledFrame, Reassembler, ReassemblyEvent};

    use super::*;

    /// Scripted screen: each queued image is one "change".
    #[derive(Default)]
    struct SourceState {
        changes: VecDeque<Vec<u8>>,
        current: Vec<u8>,
        timeouts: Vec<Duration>,
    }

    struct FakeSource(Rc<RefCell<SourceState>>);

    impl FrameSource for FakeSource {
        type Frame = Vec<u8>;

        fn wait_for_change(&mut self, timeout: Duration) -> Result<bool> {
            let mut s = self.0.borrow_mut();
            s.timeouts.push(timeout);
            match s.changes.pop_front() {
                Some(image) => {
                    s.current = image;
                    Ok(true)
                }
                None => {
                    drop(s);
                    // Mimic a real capture blocking until the timeout.
                    std::thread::sleep(timeout);
                    Ok(false)
                }
            }
        }

        fn current_frame(&mut self) -> Result<Vec<u8>> {
            Ok(self.0.borrow().current.clone())
        }
    }

    /// Encoder that "encodes" a frame as `image ++ [timestamp marker]` and
    /// holds at most `capacity` frames in flight until polled.
    struct EncoderState {
        capacity: usize,
        in_flight: VecDeque<EncodedFrame>,
        force_keyframe: bool,
        submitted: Vec<i64>,
        polls: Vec<bool>,
        /// Simulates an encoder that buffers frames and emits nothing.
        hold_output: bool,
    }

    struct FakeEncoder(Rc<RefCell<EncoderState>>);

    impl VideoEncoder for FakeEncoder {
        type Frame = Vec<u8>;

        fn can_accept(&self) -> bool {
            let s = self.0.borrow();
            s.in_flight.len() < s.capacity
        }

        fn submit(&mut self, frame: &Vec<u8>, timestamp: i64) -> Result<()> {
            let mut s = self.0.borrow_mut();
            assert!(s.in_flight.len() < s.capacity, "submit while busy");
            let keyframe = std::mem::take(&mut s.force_keyframe) || s.submitted.is_empty();
            s.submitted.push(timestamp);
            s.in_flight.push_back(EncodedFrame {
                data: frame.clone(),
                timestamp,
                keyframe,
            });
            Ok(())
        }

        fn poll(&mut self, block: bool, out: &mut VecDeque<EncodedFrame>) -> Result<()> {
            let mut s = self.0.borrow_mut();
            s.polls.push(block);
            if !s.hold_output {
                out.extend(s.in_flight.drain(..));
            }
            Ok(())
        }

        fn request_keyframe(&mut self) -> Result<()> {
            self.0.borrow_mut().force_keyframe = true;
            Ok(())
        }
    }

    struct FakeSink {
        max_size: Rc<RefCell<usize>>,
        sent: Rc<RefCell<Vec<Vec<u8>>>>,
    }

    impl DatagramSink for FakeSink {
        fn max_datagram_size(&self) -> usize {
            *self.max_size.borrow()
        }

        fn send(&self, datagram: Vec<u8>) -> Result<()> {
            self.sent.borrow_mut().push(datagram);
            Ok(())
        }
    }

    struct Harness {
        source: Rc<RefCell<SourceState>>,
        encoder: Rc<RefCell<EncoderState>>,
        max_size: Rc<RefCell<usize>>,
        sent: Rc<RefCell<Vec<Vec<u8>>>>,
        streamer: HostStreamer<FakeSource, FakeEncoder, FakeSink>,
    }

    const FPS: u32 = 1000;

    fn harness(initial: &[u8], capacity: usize) -> Harness {
        let source = Rc::new(RefCell::new(SourceState {
            current: initial.to_vec(),
            ..Default::default()
        }));
        let encoder = Rc::new(RefCell::new(EncoderState {
            capacity,
            in_flight: VecDeque::new(),
            force_keyframe: false,
            submitted: Vec::new(),
            polls: Vec::new(),
            hold_output: false,
        }));
        let max_size = Rc::new(RefCell::new(HEADER_LEN + 4));
        let sent = Rc::new(RefCell::new(Vec::new()));
        let streamer = HostStreamer::new(
            FakeSource(source.clone()),
            FakeEncoder(encoder.clone()),
            FakeSink {
                max_size: max_size.clone(),
                sent: sent.clone(),
            },
            FPS,
        );
        Harness {
            source,
            encoder,
            max_size,
            sent,
            streamer,
        }
    }

    impl Harness {
        /// Steps until `done` holds; bounded so a bug cannot hang the test.
        fn step_until(&mut self, mut done: impl FnMut(&Self) -> bool) {
            for _ in 0..1000 {
                if done(self) {
                    return;
                }
                self.streamer.step().unwrap();
            }
            panic!("condition not reached");
        }

        /// The screen changes to `image`; steps until it has been submitted.
        fn change_and_submit(&mut self, image: &[u8]) {
            let before = self.submitted();
            self.source.borrow_mut().changes.push_back(image.to_vec());
            self.step_until(|h| h.submitted() == before + 1);
        }

        fn submitted(&self) -> usize {
            self.encoder.borrow().submitted.len()
        }

        fn reassemble(&self) -> Vec<ReassembledFrame> {
            let mut r = Reassembler::new(8);
            self.sent
                .borrow()
                .iter()
                .flat_map(|d| r.push(d).unwrap())
                .map(|e| match e {
                    ReassemblyEvent::Frame(f) => f,
                    other => panic!("unexpected {other:?}"),
                })
                .collect()
        }
    }

    #[test]
    fn first_step_submits_and_sends_initial_frame_without_waiting() {
        let mut h = harness(b"screen", 1);
        h.streamer.step().unwrap();
        assert_eq!(h.submitted(), 1);
        assert!(h.source.borrow().timeouts.is_empty());
        assert_eq!(h.streamer.stats().frames_submitted, 1);
        // Sent in the same step: no wait for a screen change in between.
        assert_eq!(h.streamer.stats().frames_sent, 1);
        assert_eq!(h.encoder.borrow().polls.last(), Some(&true));
    }

    #[test]
    fn source_mut_reaches_the_owned_source() {
        let mut h = harness(b"screen", 1);
        let source = h.streamer.source_mut();
        assert!(Rc::ptr_eq(&source.0, &h.source));
        source.0.borrow_mut().current = b"changed".to_vec();
        assert_eq!(h.source.borrow().current, b"changed");
    }

    #[test]
    fn encoder_holding_frames_does_not_stall_the_loop() {
        // An encoder that never emits output must not block step() forever.
        let mut h = harness(b"screen", 4);
        h.encoder.borrow_mut().hold_output = true;
        h.streamer.step().unwrap();
        assert_eq!(h.submitted(), 1);
        assert_eq!(h.streamer.stats().frames_sent, 0);
        assert_eq!(h.encoder.borrow().polls.len(), 1 + MAX_FLUSH_EVENTS);
    }

    #[test]
    fn frames_reach_sink_and_reassemble_identically() {
        let mut h = harness(b"first image!", 1);
        h.step_until(|h| h.submitted() == 1);
        h.change_and_submit(b"second");
        h.change_and_submit(b"third image");
        h.step_until(|h| h.streamer.stats().frames_sent == 3);

        let frames = h.reassemble();
        let data: Vec<&[u8]> = frames.iter().map(|f| f.data.as_slice()).collect();
        assert_eq!(data, vec![&b"first image!"[..], b"second", b"third image"]);
        assert_eq!(
            frames.iter().map(|f| f.frame_id).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            frames.iter().map(|f| f.timestamp).collect::<Vec<_>>(),
            h.encoder.borrow().submitted
        );
        assert!(frames[0].keyframe && !frames[1].keyframe);

        let sent = h.sent.borrow();
        assert!(sent.iter().all(|d| d.len() <= HEADER_LEN + 4));
        let stats = h.streamer.stats();
        assert_eq!(stats.datagrams_sent, sent.len() as u64);
        assert_eq!(
            stats.bytes_sent,
            sent.iter().map(|d| d.len() as u64).sum::<u64>()
        );
        assert_eq!(stats.keyframes_sent, 1);
    }

    #[test]
    fn timestamps_increase_and_respect_frame_interval() {
        let mut h = harness(b"a", 1);
        h.step_until(|h| h.submitted() == 1);
        h.change_and_submit(b"b");
        h.change_and_submit(b"c");
        let ts = h.encoder.borrow().submitted.clone();
        let interval_100ns = 10_000_000 / i64::from(FPS);
        assert!(
            ts.windows(2).all(|w| w[1] - w[0] >= interval_100ns),
            "{ts:?}"
        );
    }

    #[test]
    fn max_datagram_size_is_reread_per_frame() {
        let mut h = harness(b"12345678", 1);
        h.step_until(|h| h.streamer.stats().frames_sent == 1);
        assert_eq!(h.sent.borrow().len(), 2);
        *h.max_size.borrow_mut() = HEADER_LEN + 8;
        h.change_and_submit(b"abcdefgh");
        h.step_until(|h| h.streamer.stats().frames_sent == 2);
        assert_eq!(h.sent.borrow().len(), 3);
        assert_eq!(h.reassemble()[1].data, b"abcdefgh");
    }

    #[test]
    fn static_screen_submits_nothing_and_waits_a_full_interval() {
        let mut h = harness(b"x", 1);
        h.step_until(|h| h.streamer.stats().frames_sent == 1);
        h.source.borrow_mut().timeouts.clear();
        for _ in 0..5 {
            h.streamer.step().unwrap();
        }
        assert_eq!(h.submitted(), 1);
        let interval = Duration::from_secs(1) / FPS;
        assert_eq!(h.source.borrow().timeouts, vec![interval; 5]);
    }

    #[test]
    fn keyframe_request_on_static_screen_produces_keyframe() {
        let mut h = harness(b"still", 1);
        h.step_until(|h| h.streamer.stats().frames_sent == 1);
        h.streamer.request_keyframe().unwrap();
        h.step_until(|h| h.streamer.stats().frames_sent == 2);
        assert_eq!(h.streamer.stats().keyframes_sent, 2);
        let frames = h.reassemble();
        assert!(frames[1].keyframe);
        assert_eq!(frames[1].data, b"still");
    }

    #[test]
    fn busy_encoder_is_polled_blocking_and_not_fed() {
        let mut h = harness(b"a", 1);
        h.encoder.borrow_mut().hold_output = true;
        h.streamer.step().unwrap();
        assert_eq!(h.submitted(), 1);
        h.encoder.borrow_mut().polls.clear();
        h.source.borrow_mut().changes.push_back(b"b".to_vec());
        h.streamer.step().unwrap();
        // Encoder still full: the poll blocked and nothing new was submitted.
        assert!(h.encoder.borrow().polls[0]);
        assert_eq!(h.submitted(), 1);
        // Once it releases the frame, it is sent.
        h.encoder.borrow_mut().hold_output = false;
        h.streamer.step().unwrap();
        assert!(h.streamer.stats().frames_sent >= 1);
    }

    #[test]
    fn faster_changes_than_fps_send_latest_image() {
        let mut h = harness(b"a", 4);
        h.streamer.step().unwrap();
        // Many changes arrive at once; the pacer coalesces them.
        h.source
            .borrow_mut()
            .changes
            .extend((b'b'..=b'z').map(|c| vec![c]));
        h.step_until(|h| h.source.borrow().changes.is_empty() && !h.streamer.pacer.is_dirty());
        h.step_until(|h| h.streamer.stats().frames_sent == h.submitted() as u64);
        let frames = h.reassemble();
        assert!(
            frames.len() < 26,
            "changes should be coalesced, got {}",
            frames.len()
        );
        assert_eq!(frames.last().unwrap().data, b"z");
    }
}
