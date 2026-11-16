//! The abstractions every platform adapter implements. CONTRACT: do not
//! change these signatures without updating every implementor.

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;

/// One encoded access unit in Annex-B format (start codes included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    pub data: Bytes,
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

    /// Changes the target bitrate of the frames encoded from now on, while
    /// streaming. An error means the encoder keeps its previous rate.
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()>;
}

/// Sends unreliable datagrams (host side).
pub trait DatagramSink {
    /// Largest datagram the transport can currently carry, in bytes.
    fn max_datagram_size(&self) -> usize;

    /// Sends one datagram. May silently drop it under congestion. Taking
    /// `Bytes` lets a frame's datagrams share one allocation.
    ///
    /// Fails with a [`DatagramTooLarge`] (found with `anyhow::Error::is`)
    /// when the datagram exceeds the current limit, which may have shrunk
    /// since the datagram was sized: that datagram is lost, but the
    /// transport still works. Any other error means it cannot send anymore.
    fn send(&self, datagram: Bytes) -> Result<()>;

    /// Bytes handed to [`Self::send`] that are still waiting to leave. It
    /// grows while the network is slower than the stream, so the sender can
    /// stop producing instead of queueing stale video.
    fn backlog_bytes(&self) -> usize;
}

/// A datagram larger than the transport's current limit
/// ([`DatagramSink::max_datagram_size`]). The limit can shrink while
/// streaming (QUIC lowers it when it suspects the path's MTU after a burst
/// of losses), so a datagram sized for the old limit may no longer fit;
/// the transport itself is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatagramTooLarge {
    /// Size of the datagram refused, in bytes.
    pub len: usize,
    /// The limit at the time, in bytes.
    pub limit: usize,
}

impl fmt::Display for DatagramTooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "datagram of {} bytes exceeds the current limit of {}",
            self.len, self.limit
        )
    }
}

impl std::error::Error for DatagramTooLarge {}

/// Synchronous video decoder (viewer side).
pub trait VideoDecoder {
    type Frame;

    /// Decodes one complete access unit. Returns `None` while the decoder
    /// needs more data before producing a picture.
    fn decode(&mut self, unit: &[u8], timestamp: i64) -> Result<Option<Self::Frame>>;

    /// Forgets the stream followed so far, e.g. before a new connection to
    /// the host starts a new one: pictures and references of the old stream
    /// must not leak into it. The next unit must be a keyframe. Decoders
    /// without state to drop keep this default.
    fn reset(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Shows decoded frames (viewer side).
pub trait FrameSink {
    type Frame;

    fn present(&mut self, frame: &Self::Frame) -> Result<()>;
}
