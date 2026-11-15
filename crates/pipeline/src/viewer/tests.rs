//! Tests of [`super::ViewerStreamer`] with a fake decoder and renderer.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::bail;
use protocol::recovery::Missing;
use protocol::video::{HEADER_LEN, Packetizer};

use super::*;
use crate::recovery::{TAIL_GRACE, recovery_wait};

/// Decoder producing `(data, timestamp)`; returns `None` for empty units
/// (decoder needs more data) and fails on the unit `b"boom"`.
struct FakeDecoder(Rc<RefCell<Vec<Vec<u8>>>>);

impl VideoDecoder for FakeDecoder {
    type Frame = (Vec<u8>, i64);

    fn decode(&mut self, unit: &[u8], timestamp: i64) -> Result<Option<Self::Frame>> {
        if unit == b"boom" {
            bail!("decoder failure");
        }
        self.0.borrow_mut().push(unit.to_vec());
        Ok((!unit.is_empty()).then(|| (unit.to_vec(), timestamp)))
    }
}

type Presented = Rc<RefCell<Vec<(Vec<u8>, i64)>>>;

struct FakeRenderer(Presented);

impl FrameSink for FakeRenderer {
    type Frame = (Vec<u8>, i64);

    fn present(&mut self, frame: &Self::Frame) -> Result<()> {
        self.0.borrow_mut().push(frame.clone());
        Ok(())
    }
}

struct Harness {
    decoded: Rc<RefCell<Vec<Vec<u8>>>>,
    presented: Presented,
    viewer: ViewerStreamer<FakeDecoder, FakeRenderer>,
    packetizer: Packetizer,
    base: Instant,
}

/// Four payload bytes per datagram.
const MAX: usize = HEADER_LEN + 4;

/// A moment well into the stream, past any keyframe request made when it
/// started, so a request at a loss is not held back by the rate limit.
const LATER: u64 = 300;

fn harness() -> Harness {
    let decoded = Rc::new(RefCell::new(Vec::new()));
    let presented = Rc::new(RefCell::new(Vec::new()));
    let viewer = ViewerStreamer::new(
        FakeDecoder(decoded.clone()),
        FakeRenderer(presented.clone()),
    );
    Harness {
        decoded,
        presented,
        viewer,
        packetizer: Packetizer::new(),
        base: Instant::now(),
    }
}

impl Harness {
    fn at(&self, ms: u64) -> Instant {
        self.base + Duration::from_millis(ms)
    }

    fn packets(&mut self, data: &[u8], keyframe: bool, ts: i64) -> Vec<Bytes> {
        self.packetizer.packetize(data, keyframe, ts, MAX).unwrap()
    }

    /// Feeds the datagrams one by one at time `ms`; all the actions.
    fn feed(&mut self, datagrams: &[Bytes], ms: u64) -> Vec<ViewerAction> {
        let now = self.at(ms);
        datagrams
            .iter()
            .flat_map(|d| self.viewer.on_datagrams([d.clone()], now).unwrap())
            .collect()
    }

    fn send(&mut self, data: &[u8], keyframe: bool, ms: u64) -> Vec<ViewerAction> {
        let packets = self.packets(data, keyframe, ms as i64);
        self.feed(&packets, ms)
    }

    fn tick(&mut self, now: Instant) -> Vec<ViewerAction> {
        self.viewer.on_tick(now).unwrap()
    }

    /// Frame 0, a keyframe, at time 0: the stream is running.
    fn started() -> Self {
        let mut h = harness();
        h.send(b"key", true, 0);
        h
    }

    fn requests(actions: &[ViewerAction]) -> usize {
        actions
            .iter()
            .filter(|a| **a == ViewerAction::RequestKeyframe)
            .count()
    }

    fn last_presented(&self) -> Vec<u8> {
        self.presented.borrow().last().unwrap().0.clone()
    }
}

fn nack(frame_id: u32, missing: Missing) -> ViewerAction {
    ViewerAction::Nack(Nack { frame_id, missing })
}

#[test]
fn decodes_and_presents_complete_frames() {
    let mut h = harness();
    let actions = h.send(b"keyframe", true, 0);
    // The first datagram arrives while awaiting a keyframe: one request.
    assert_eq!(actions, vec![ViewerAction::RequestKeyframe]);
    let actions = h.send(b"delta", false, 10);
    assert!(actions.is_empty());
    assert_eq!(
        *h.presented.borrow(),
        vec![(b"keyframe".to_vec(), 0), (b"delta".to_vec(), 10)]
    );
    let stats = h.viewer.stats();
    assert_eq!((stats.frames_decoded, stats.frames_presented), (2, 2));
    assert_eq!(stats.keyframes_requested, 1);
    assert_eq!(h.viewer.next_wakeup(), None);
}

#[test]
fn decoder_without_output_is_not_presented() {
    let mut h = harness();
    h.send(b"", true, 0);
    assert_eq!(h.decoded.borrow().len(), 1);
    assert!(h.presented.borrow().is_empty());
    assert_eq!(h.viewer.stats().frames_decoded, 0);
}

#[test]
fn frames_before_first_keyframe_are_skipped() {
    let mut h = harness();
    h.send(b"delta", false, 0);
    assert!(h.decoded.borrow().is_empty());
    assert_eq!(h.viewer.stats().frames_skipped, 1);
    h.send(b"key", true, LATER);
    assert_eq!(h.presented.borrow().len(), 1);
}

#[test]
fn an_isolated_loss_is_recovered_by_a_nack_without_a_keyframe() {
    let mut h = Harness::started();
    // Frame 1 loses its middle datagram; frame 2 arrives whole.
    let lossy = h.packets(b"lost frame", false, 1);
    assert!(
        h.feed(&[lossy[0].clone(), lossy[2].clone()], LATER)
            .is_empty()
    );
    let actions = h.send(b"next", false, LATER + 1);
    assert_eq!(actions, vec![nack(1, Missing::Packets(vec![1]))]);
    // Frame 2 waits for frame 1: nothing new on screen yet.
    assert_eq!(h.presented.borrow().len(), 1);

    // The resend completes frame 1; both are decoded in order, the newest
    // is shown.
    let actions = h.feed(&lossy[1..2], LATER + 3);
    assert!(actions.is_empty(), "{actions:?}");
    assert_eq!(*h.decoded.borrow(), [&b"key"[..], b"lost frame", b"next"]);
    assert_eq!(h.last_presented(), b"next");
    let stats = h.viewer.stats();
    assert_eq!(stats.frames_recovered, 1);
    assert_eq!(stats.nacks_sent, 1);
    assert_eq!((stats.frames_lost, stats.frames_skipped), (0, 0));
    assert_eq!(stats.keyframes_requested, 0);
    assert_eq!(h.viewer.next_wakeup(), None);
}

#[test]
fn an_unanswered_nack_falls_back_to_a_keyframe() {
    let mut h = Harness::started();
    let lossy = h.packets(b"lost frame", false, 1);
    h.feed(&lossy[..1], LATER);
    let asked_at = h.at(LATER + 1);
    assert_eq!(
        h.send(b"next", false, LATER + 1),
        vec![nack(1, Missing::Packets(vec![1, 2]))]
    );
    let deadline = asked_at + recovery_wait(ASSUMED_RTT);
    assert_eq!(h.viewer.next_wakeup(), Some(deadline));

    // Until the deadline the viewer keeps waiting.
    assert!(h.tick(deadline - Duration::from_millis(1)).is_empty());
    assert_eq!(h.viewer.stats().frames_lost, 0);
    // Then it gives up: frame 1 is lost, frame 2 cannot be decoded.
    assert_eq!(h.tick(deadline), vec![ViewerAction::RequestKeyframe]);
    let stats = h.viewer.stats().clone();
    assert_eq!((stats.frames_lost, stats.frames_skipped), (1, 1));
    assert_eq!(stats.frames_recovered, 0);
    assert_eq!(h.presented.borrow().len(), 1);
    // A resend arriving after that changes nothing.
    assert!(h.feed(&lossy[1..], LATER + 100).is_empty());
    assert_eq!(h.viewer.stats().frames_lost, 1);
    // The keyframe resolves it.
    h.send(b"key2", true, LATER + 120);
    assert_eq!(h.last_presented(), b"key2");
}

#[test]
fn keyframe_requests_after_a_loss_are_rate_limited() {
    let mut h = Harness::started();
    let lossy = h.packets(b"lost frame", false, 1);
    h.feed(&lossy[..1], LATER);
    h.send(b"next", false, LATER);
    let gave_up = h.at(LATER) + recovery_wait(ASSUMED_RTT);
    assert_eq!(Harness::requests(&h.tick(gave_up)), 1);
    // Still awaiting a keyframe: asked again only after the interval.
    assert_eq!(
        Harness::requests(&h.tick(gave_up + Duration::from_millis(100))),
        0
    );
    assert_eq!(
        Harness::requests(&h.tick(gave_up + KEYFRAME_REQUEST_INTERVAL)),
        1
    );
    assert_eq!(h.viewer.stats().keyframes_requested, 2);
}

#[test]
fn the_newest_frame_is_asked_for_after_a_short_silence() {
    let mut h = Harness::started();
    // The last datagram of the last frame is lost and nothing follows (a
    // static screen): only the silence tells.
    let lossy = h.packets(b"tail lost!", false, 1);
    assert!(h.feed(&lossy[..2], LATER).is_empty());
    let due = h.at(LATER) + TAIL_GRACE;
    assert_eq!(h.viewer.next_wakeup(), Some(due));
    assert!(h.tick(due - Duration::from_millis(1)).is_empty());
    assert_eq!(h.tick(due), vec![nack(1, Missing::Packets(vec![2]))]);
    h.feed(&lossy[2..], LATER + 6);
    assert_eq!(h.last_presented(), b"tail lost!");
    assert_eq!(h.viewer.stats().frames_recovered, 1);
}

#[test]
fn a_frame_lost_entirely_is_asked_for_whole() {
    let mut h = Harness::started();
    let _dropped = h.packets(b"gone", false, 1);
    assert_eq!(h.send(b"next", false, LATER), vec![nack(1, Missing::Whole)]);
}

#[test]
fn the_wait_follows_the_round_trip_time() {
    let mut h = Harness::started();
    let rtt = Duration::from_millis(40);
    h.viewer.set_rtt(rtt);
    let lossy = h.packets(b"lost frame", false, 1);
    h.feed(&lossy[..1], LATER);
    h.send(b"next", false, LATER);
    assert_eq!(
        h.viewer.next_wakeup(),
        Some(h.at(LATER) + recovery_wait(rtt))
    );
}

#[test]
fn tick_rerequests_while_awaiting_keyframe() {
    let mut h = harness();
    assert_eq!(h.tick(h.at(0)), vec![ViewerAction::RequestKeyframe]);
    assert!(h.tick(h.at(100)).is_empty());
    assert!(h.tick(h.at(200)).is_empty());
    assert_eq!(h.tick(h.at(300)), vec![ViewerAction::RequestKeyframe]);
    h.send(b"key", true, 400);
    assert!(h.tick(h.at(1000)).is_empty());
    assert_eq!(h.viewer.stats().keyframes_requested, 2);
}

#[test]
fn malformed_datagram_is_ignored() {
    let mut h = Harness::started();
    let actions = h
        .viewer
        .on_datagrams([Bytes::from_static(&[1, 2, 3])], h.at(10))
        .unwrap();
    assert!(actions.is_empty());
    assert_eq!(h.viewer.stats().frames_lost, 0);
}

#[test]
fn decoder_error_is_propagated() {
    let mut h = harness();
    let packets = h.packets(b"boom", true, 0);
    assert!(
        h.viewer
            .on_datagrams([packets[0].clone()], h.at(0))
            .is_err()
    );
}

#[test]
fn backlog_is_decoded_whole_but_only_the_latest_frame_is_presented() {
    let mut h = Harness::started();
    let mut backlog = Vec::new();
    for (ts, data) in [(1, &b"d1"[..]), (2, b"d2"), (3, b"d3")] {
        backlog.extend(h.packets(data, false, ts));
    }
    let actions = h.viewer.on_datagrams(backlog, h.at(50)).unwrap();
    assert!(actions.is_empty());
    // Each frame is a reference for the next: all of them are decoded.
    assert_eq!(*h.decoded.borrow(), [&b"key"[..], b"d1", b"d2", b"d3"]);
    assert_eq!(
        *h.presented.borrow(),
        vec![(b"key".to_vec(), 0), (b"d3".to_vec(), 3)]
    );
    let stats = h.viewer.stats();
    assert_eq!(stats.frames_decoded, 4);
    assert_eq!(stats.frames_presented, 2);
    assert_eq!(stats.frames_superseded, 2);
}

#[test]
fn batch_completing_no_frame_presents_nothing() {
    let mut h = Harness::started();
    let partial = h.packets(b"split frame", false, 1);
    let actions = h.viewer.on_datagrams([partial[0].clone()], h.at(10));
    assert!(actions.unwrap().is_empty());
    assert_eq!(h.presented.borrow().len(), 1);
    assert_eq!(h.viewer.stats().frames_superseded, 0);
    assert!(h.viewer.on_datagrams([], h.at(11)).unwrap().is_empty());
}

#[test]
fn loss_inside_a_batch_is_asked_for_at_once() {
    let mut h = Harness::started();
    let lost = h.packets(b"lost frame", false, 1);
    let next = h.packets(b"next", false, 2);
    let batch = [lost[0].clone(), next[0].clone()];
    let actions = h.viewer.on_datagrams(batch, h.at(LATER)).unwrap();
    assert_eq!(actions, vec![nack(1, Missing::Packets(vec![1, 2]))]);
    assert_eq!(h.viewer.stats().frames_lost, 0);
    assert_eq!(h.presented.borrow().len(), 1, "nothing decodable arrived");
}
