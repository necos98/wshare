//! Tests of [`super::HostStreamer`] with a scripted screen, encoder and
//! sink.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use bytes::Bytes;
use protocol::video::{HEADER_LEN, ReassembledFrame, Reassembler, ReassemblyEvent};

use super::*;

/// Scripted screen: each queued image is one "change".
#[derive(Default)]
struct SourceState {
    changes: VecDeque<Vec<u8>>,
    current: Vec<u8>,
    timeouts: Vec<Duration>,
    /// Calls to `current_frame`: each one is a capture.
    captures: usize,
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
        let mut s = self.0.borrow_mut();
        s.captures += 1;
        Ok(s.current.clone())
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
    /// Last rate accepted by `set_bitrate`.
    bitrate: Option<u32>,
    /// Simulates a driver that cannot change the rate while streaming.
    refuse_bitrate: bool,
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
            data: Bytes::copy_from_slice(frame),
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

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        let mut s = self.0.borrow_mut();
        if s.refuse_bitrate {
            anyhow::bail!("rate change not supported");
        }
        s.bitrate = Some(bitrate_bps);
        Ok(())
    }
}

/// Records every datagram and counts the bytes of each as queued until
/// the test drains them, like a link that has not sent them yet.
struct FakeSink {
    max_size: Rc<RefCell<usize>>,
    sent: Rc<RefCell<Vec<Bytes>>>,
    backlog: Rc<Cell<usize>>,
}

impl DatagramSink for FakeSink {
    fn max_datagram_size(&self) -> usize {
        *self.max_size.borrow()
    }

    fn send(&self, datagram: Bytes) -> Result<()> {
        self.backlog.set(self.backlog.get() + datagram.len());
        self.sent.borrow_mut().push(datagram);
        Ok(())
    }

    fn backlog_bytes(&self) -> usize {
        self.backlog.get()
    }
}

struct Harness {
    source: Rc<RefCell<SourceState>>,
    encoder: Rc<RefCell<EncoderState>>,
    max_size: Rc<RefCell<usize>>,
    sent: Rc<RefCell<Vec<Bytes>>>,
    backlog: Rc<Cell<usize>>,
    streamer: HostStreamer<FakeSource, FakeEncoder, FakeSink>,
}

const FPS: u32 = 1000;
/// 1 000 bytes per frame at [`FPS`], so a 3 000-byte backlog limit:
/// tests about other behaviour send far less than that in total.
const BITRATE: u32 = 8_000_000;

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
        bitrate: None,
        refuse_bitrate: false,
    }));
    let max_size = Rc::new(RefCell::new(HEADER_LEN + 4));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let backlog = Rc::new(Cell::new(0));
    let streamer = HostStreamer::new(
        FakeSource(source.clone()),
        FakeEncoder(encoder.clone()),
        FakeSink {
            max_size: max_size.clone(),
            sent: sent.clone(),
            backlog: backlog.clone(),
        },
        FPS,
        BITRATE,
    );
    Harness {
        source,
        encoder,
        max_size,
        sent,
        backlog,
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
            .flat_map(|d| r.push(d.clone()).unwrap())
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
    let data: Vec<&[u8]> = frames.iter().map(|f| &f.data[..]).collect();
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
    // A frame may land up to the pacer tolerance (a quarter interval at this
    // rate) ahead of its slot.
    let earliest_gap = interval_100ns - interval_100ns / 4;
    assert!(ts.windows(2).all(|w| w[1] - w[0] >= earliest_gap), "{ts:?}");
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
    assert_eq!(h.reassemble()[1].data[..], *b"abcdefgh");
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
    assert_eq!(frames[1].data[..], *b"still");
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
    assert_eq!(frames.last().unwrap().data[..], *b"z");
}

#[test]
fn congested_sink_skips_frames_before_capture_and_encoding() {
    let mut h = harness(b"first", 1);
    h.step_until(|h| h.streamer.stats().frames_sent == 1);
    h.backlog.set(send_backlog_limit(BITRATE, FPS) + 1);
    let captures = h.source.borrow().captures;
    h.source.borrow_mut().timeouts.clear();
    h.source.borrow_mut().changes.push_back(b"second".to_vec());
    h.step_until(|h| h.streamer.stats().frames_skipped == 3);
    assert_eq!(h.submitted(), 1);
    assert_eq!(h.source.borrow().captures, captures, "nothing captured");
    // Each skipped slot waits for the next one: no busy loop.
    let timeouts = h.source.borrow().timeouts.clone();
    assert!(timeouts.len() >= 3 && timeouts.iter().all(|t| !t.is_zero()));

    // Once the queue drains, the latest image goes out at the next slot.
    h.backlog.set(0);
    h.step_until(|h| h.streamer.stats().frames_sent == 2);
    assert_eq!(h.streamer.stats().frames_skipped, 3);
    assert_eq!(h.reassemble()[1].data[..], *b"second");
}

#[test]
fn a_lower_bitrate_reaches_the_encoder_and_shrinks_the_backlog_allowed() {
    let mut h = harness(b"first", 1);
    h.step_until(|h| h.streamer.stats().frames_sent == 1);
    h.streamer.set_bitrate(BITRATE / 2).unwrap();
    assert_eq!(h.encoder.borrow().bitrate, Some(BITRATE / 2));
    // Within the old limit, over the new one.
    h.backlog.set(send_backlog_limit(BITRATE / 2, FPS) + 1);
    h.source.borrow_mut().changes.push_back(b"second".to_vec());
    h.step_until(|h| h.streamer.stats().frames_skipped == 1);
    assert_eq!(h.submitted(), 1);
}

#[test]
fn a_refused_bitrate_changes_nothing() {
    let mut h = harness(b"first", 1);
    h.step_until(|h| h.streamer.stats().frames_sent == 1);
    h.encoder.borrow_mut().refuse_bitrate = true;
    assert!(h.streamer.set_bitrate(BITRATE / 2).is_err());
    assert_eq!(h.encoder.borrow().bitrate, None);
    // The backlog allowed still matches the rate the encoder kept.
    h.backlog.set(send_backlog_limit(BITRATE / 2, FPS) + 1);
    h.change_and_submit(b"second");
    assert_eq!(h.streamer.stats().frames_skipped, 0);
}

#[test]
fn backlog_stays_bounded_when_the_link_is_slower_than_the_stream() {
    /// Bytes the fake link sends per frame slot: a quarter of a frame.
    const DRAIN_PER_SLOT: usize = 118;
    let mut h = harness(b"start", 1);
    *h.max_size.borrow_mut() = HEADER_LEN + 100;
    // 400 bytes in 4 datagrams of `HEADER_LEN + 100`.
    let frame_bytes = 4 * (HEADER_LEN + 100);
    let slots = |h: &Harness| {
        let stats = h.streamer.stats();
        stats.frames_submitted + stats.frames_skipped
    };
    let mut peak = 0;
    for i in 0..200u32 {
        // The screen changes every slot; each iteration lasts one slot.
        let before = slots(&h);
        h.source.borrow_mut().changes.push_back(vec![i as u8; 400]);
        h.step_until(|h| slots(h) > before);
        peak = peak.max(h.backlog.get());
        h.backlog
            .set(h.backlog.get().saturating_sub(DRAIN_PER_SLOT));
    }
    let stats = h.streamer.stats().clone();
    assert!(stats.frames_skipped > 0, "{stats:?}");
    let limit = send_backlog_limit(BITRATE, FPS);
    assert!(peak <= limit + frame_bytes, "peak {peak} over {limit}");
    // Every frame that was encoded went out whole.
    assert_eq!(stats.frames_sent, stats.frames_submitted);
    assert_eq!(h.reassemble().len() as u64, stats.frames_sent);
}
