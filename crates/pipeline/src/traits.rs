//! The abstractions every platform adapter implements. CONTRACT: do not
//! change these signatures without updating every implementor.

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::Result;

/// One encoded access unit in Annex-B format (start codes included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    /// Presentation timestamp in 100 ns units (Media Foundation convention).
    pub timestamp: i64,
    pub keyframe: bool,
}

/// Produces encoder-ready frames of the screen (host side).
pub trait FrameSource {
    /// Opaque frame handle (e.g. an NV12 GPU texture).
    type Frame;

    /// Waits up to `timeout` for the screen to change. Returns `true` when a
    /// new image was captured; it becomes the image [`Self::current_frame`]
    /// returns.
    fn wait_for_change(&mut self, timeout: Duration) -> Result<bool>;

    /// Returns an encoder-ready frame built from the latest captured image.
    fn current_frame(&mut self) -> Result<Self::Frame>;
}

/// Asynchronous video encoder (host side).
pub trait VideoEncoder {
    type Frame;

    /// True when the encoder is ready to take another frame.
    fn can_accept(&self) -> bool;

    /// Queues a frame. Only valid when [`Self::can_accept`] is true.
    /// `timestamp` is in 100 ns units.
    fn submit(&mut self, frame: &Self::Frame, timestamp: i64) -> Result<()>;

    /// Processes encoder events and appends finished frames to `out`. With
    /// `block`, waits until at least one event happened.
    fn poll(&mut self, block: bool, out: &mut VecDeque<EncodedFrame>) -> Result<()>;

    /// Makes the next encoded frame a keyframe.
    fn request_keyframe(&mut self) -> Result<()>;
}

/// Sends unreliable datagrams (host side).
pub trait DatagramSink {
    /// Largest datagram the transport can currently carry, in bytes.
    fn max_datagram_size(&self) -> usize;

    /// Sends one datagram. May silently drop it under congestion.
    fn send(&self, datagram: Vec<u8>) -> Result<()>;
}

/// Synchronous video decoder (viewer side).
pub trait VideoDecoder {
    type Frame;

    /// Decodes one complete access unit. Returns `None` while the decoder
    /// needs more data before producing a picture.
    fn decode(&mut self, unit: &[u8], timestamp: i64) -> Result<Option<Self::Frame>>;
}

/// Shows decoded frames (viewer side).
pub trait FrameSink {
    type Frame;

    fn present(&mut self, frame: &Self::Frame) -> Result<()>;
}
