//! Viewer loop: datagrams -> reassemble -> decode -> present.
//!
//! The app's network task receives datagrams and hands them to
//! [`ViewerStreamer::on_datagram`]; the returned action tells it whether to
//! send a keyframe request on the control channel.

use std::time::{Duration, Instant};

use anyhow::Result;
use protocol::video::{KeyframeRequester, ReassembledFrame, Reassembler, ReassemblyEvent};

use crate::traits::{FrameSink, VideoDecoder};

/// Incomplete frames kept while waiting for their remaining datagrams. At
/// 60 fps this is ~130 ms of video, far more than LAN reordering needs.
pub const MAX_PENDING_FRAMES: usize = 8;

/// Minimum time between two keyframe requests: long enough for the host to
/// answer one request (encode + send a keyframe) on a LAN, short enough that
/// a lost request is retried before the user notices a frozen picture.
pub const KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerAction {
    None,
    /// Send `ControlMessage::RequestKeyframe` to the host.
    RequestKeyframe,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewerStats {
    pub frames_decoded: u64,
    pub frames_presented: u64,
    pub frames_lost: u64,
    /// Complete frames thrown away because they depend on a lost frame.
    pub frames_skipped: u64,
    pub keyframes_requested: u64,
}

pub struct ViewerStreamer<Dec, Sink>
where
    Dec: VideoDecoder,
    Sink: FrameSink<Frame = Dec::Frame>,
{
    reassembler: Reassembler,
    keyframes: KeyframeRequester,
    decoder: Dec,
    sink: Sink,
    stats: ViewerStats,
}

impl<Dec, Sink> ViewerStreamer<Dec, Sink>
where
    Dec: VideoDecoder,
    Sink: FrameSink<Frame = Dec::Frame>,
{
    /// Uses [`MAX_PENDING_FRAMES`] and [`KEYFRAME_REQUEST_INTERVAL`].
    pub fn new(decoder: Dec, sink: Sink) -> Self {
        Self {
            reassembler: Reassembler::new(MAX_PENDING_FRAMES),
            keyframes: KeyframeRequester::new(KEYFRAME_REQUEST_INTERVAL),
            decoder,
            sink,
            stats: ViewerStats::default(),
        }
    }

    /// Feeds one datagram. Complete frames are decoded and presented
    /// immediately. Returns [`ViewerAction::RequestKeyframe`] when frames
    /// were lost (or we are still waiting for a keyframe) and the
    /// [`KeyframeRequester`] allows a new request at `now`.
    ///
    /// Malformed datagrams are ignored (a single corrupt datagram must not
    /// end the session); errors come only from the decoder or the sink.
    pub fn on_datagram(&mut self, datagram: &[u8], now: Instant) -> Result<ViewerAction> {
        let Ok(events) = self.reassembler.push(datagram) else {
            return Ok(ViewerAction::None);
        };
        let mut lost_any = false;
        for event in events {
            match event {
                ReassemblyEvent::Frame(frame) => self.decode_and_present(&frame)?,
                ReassemblyEvent::Lost { .. } => {
                    self.stats.frames_lost += 1;
                    lost_any = true;
                }
                ReassemblyEvent::Skipped { .. } => self.stats.frames_skipped += 1,
            }
        }
        Ok(self.request_keyframe_if(lost_any || self.reassembler.awaiting_keyframe(), now))
    }

    /// Call periodically (e.g. every 100 ms) even without datagrams, so a
    /// still-missing keyframe is requested again.
    pub fn on_tick(&mut self, now: Instant) -> ViewerAction {
        self.request_keyframe_if(self.reassembler.awaiting_keyframe(), now)
    }

    /// Access to the sink for presentation concerns outside the stream
    /// (e.g. the window was resized and the last picture must be redrawn).
    pub fn sink_mut(&mut self) -> &mut Sink {
        &mut self.sink
    }

    pub fn stats(&self) -> &ViewerStats {
        &self.stats
    }

    fn decode_and_present(&mut self, frame: &ReassembledFrame) -> Result<()> {
        if let Some(picture) = self.decoder.decode(&frame.data, frame.timestamp)? {
            self.stats.frames_decoded += 1;
            self.sink.present(&picture)?;
            self.stats.frames_presented += 1;
        }
        Ok(())
    }

    fn request_keyframe_if(&mut self, needed: bool, now: Instant) -> ViewerAction {
        if needed && self.keyframes.should_request(now) {
            self.stats.keyframes_requested += 1;
            ViewerAction::RequestKeyframe
        } else {
            ViewerAction::None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use anyhow::bail;
    use protocol::video::{HEADER_LEN, Packetizer};

    use super::*;

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

    const MAX: usize = HEADER_LEN + 4;

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

        fn packets(&mut self, data: &[u8], keyframe: bool, ts: i64) -> Vec<Vec<u8>> {
            self.packetizer.packetize(data, keyframe, ts, MAX).unwrap()
        }

        /// Feeds all datagrams at time `ms`, returning the actions.
        fn feed(&mut self, datagrams: &[Vec<u8>], ms: u64) -> Vec<ViewerAction> {
            let now = self.at(ms);
            datagrams
                .iter()
                .map(|d| self.viewer.on_datagram(d, now).unwrap())
                .collect()
        }

        fn send(&mut self, data: &[u8], keyframe: bool, ms: u64) -> Vec<ViewerAction> {
            let packets = self.packets(data, keyframe, ms as i64);
            self.feed(&packets, ms)
        }

        fn requests(actions: &[ViewerAction]) -> usize {
            actions
                .iter()
                .filter(|a| **a == ViewerAction::RequestKeyframe)
                .count()
        }
    }

    #[test]
    fn decodes_and_presents_complete_frames() {
        let mut h = harness();
        let actions = h.send(b"keyframe", true, 0);
        // The first datagram arrives while awaiting a keyframe: one request.
        assert_eq!(
            actions,
            vec![ViewerAction::RequestKeyframe, ViewerAction::None]
        );
        let actions = h.send(b"delta", false, 10);
        assert_eq!(Harness::requests(&actions), 0);
        assert_eq!(
            *h.presented.borrow(),
            vec![(b"keyframe".to_vec(), 0), (b"delta".to_vec(), 10)]
        );
        let stats = h.viewer.stats();
        assert_eq!((stats.frames_decoded, stats.frames_presented), (2, 2));
        assert_eq!(stats.keyframes_requested, 1);
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
        h.send(b"key", true, 300);
        assert_eq!(h.presented.borrow().len(), 1);
    }

    #[test]
    fn loss_requests_keyframe_once_then_rate_limited() {
        let mut h = harness();
        h.send(b"key", true, 0);
        let lost = h.packets(b"lost frame", false, 1);
        h.feed(&lost[..1], 300);
        // Next frame completes: the previous one is lost -> one request.
        assert_eq!(Harness::requests(&h.send(b"next", false, 300)), 1);
        assert_eq!(h.viewer.stats().frames_lost, 1);
        assert_eq!(h.viewer.stats().frames_skipped, 1);
        // Further datagrams/losses inside the interval do not request again.
        let partial = h.packets(b"another lost", false, 2);
        h.feed(&partial[..1], 400);
        assert_eq!(Harness::requests(&h.send(b"more", false, 450)), 0);
        assert_eq!(h.viewer.stats().frames_lost, 2);
        // After the interval, still awaiting a keyframe -> request again.
        assert_eq!(Harness::requests(&h.send(b"later", false, 550)), 1);
        assert_eq!(h.viewer.stats().keyframes_requested, 2);
        // The keyframe resolves it.
        assert_eq!(Harness::requests(&h.send(b"key2", true, 900)), 0);
        assert_eq!(h.presented.borrow().last().unwrap().0, b"key2");
    }

    #[test]
    fn tick_rerequests_while_awaiting_keyframe() {
        let mut h = harness();
        assert_eq!(h.viewer.on_tick(h.at(0)), ViewerAction::RequestKeyframe);
        assert_eq!(h.viewer.on_tick(h.at(100)), ViewerAction::None);
        assert_eq!(h.viewer.on_tick(h.at(200)), ViewerAction::None);
        assert_eq!(h.viewer.on_tick(h.at(300)), ViewerAction::RequestKeyframe);
        h.send(b"key", true, 400);
        assert_eq!(h.viewer.on_tick(h.at(1000)), ViewerAction::None);
        assert_eq!(h.viewer.stats().keyframes_requested, 2);
    }

    #[test]
    fn malformed_datagram_is_ignored() {
        let mut h = harness();
        h.send(b"key", true, 0);
        assert_eq!(
            h.viewer.on_datagram(&[1, 2, 3], h.at(10)).unwrap(),
            ViewerAction::None
        );
        assert_eq!(h.viewer.stats().frames_lost, 0);
    }

    #[test]
    fn decoder_error_is_propagated() {
        let mut h = harness();
        let packets = h.packets(b"boom", true, 0);
        assert!(h.viewer.on_datagram(&packets[0], h.at(0)).is_err());
    }
}
