//! Host loop: capture -> encode -> packetize -> send.
//!
//! Runs on a dedicated (blocking) thread; the app calls [`HostStreamer::step`]
//! in a loop. Keyframe requests arriving from the network are forwarded with
//! [`HostStreamer::request_keyframe`].
//!
//! Under congestion frames are skipped *before* capture and encoding (see
//! [`crate::backlog`]): every frame that is encoded is also sent whole, so
//! the decoder's reference chain stays valid.

use std::collections::VecDeque;
use std::time::Instant;

use anyhow::Result;
use protocol::video::Packetizer;

use crate::backlog::send_backlog_limit;
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
    /// Due frames not captured because the send queue was over its limit.
    pub frames_skipped: u64,
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
    /// Send backlog, in bytes, above which due frames are skipped.
    backlog_limit: usize,
    stats: HostStats,
}

impl<S, E, D> HostStreamer<S, E, D>
where
    S: FrameSource,
    E: VideoEncoder<Frame = S::Frame>,
    D: DatagramSink,
{
    /// The pacer starts dirty so the first `step` sends an initial picture
    /// without waiting for the screen to change. `bitrate_bps`, the
    /// encoder's target rate, sizes the send backlog allowed (see
    /// [`send_backlog_limit`]). Panics if `fps` is 0.
    pub fn new(source: S, encoder: E, sink: D, fps: u32, bitrate_bps: u32) -> Self {
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
            backlog_limit: send_backlog_limit(bitrate_bps, fps),
            stats: HostStats::default(),
        }
    }

    /// One loop iteration:
    /// 1. poll the encoder (blocking only if it cannot accept input) and
    ///    send every finished frame, packetized to `sink.max_datagram_size()`
    ///    (re-read for every frame: the path MTU can change);
    /// 2. if not due yet, wait for a screen change up to the pacer timeout;
    /// 3. if the pacer says so and the encoder can accept, submit the
    ///    current frame with a timestamp relative to `start` — unless the
    ///    sink's backlog is over the limit: the slot is then skipped and the
    ///    image retried at the next one.
    ///
    /// No busy loop: when nothing is due, step 2 blocks for up to one
    /// interval (static screen) or until the next slot (dirty or skipped);
    /// when a frame is due but the encoder is busy, step 1 of the next call
    /// blocks.
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
        if self.sink.backlog_bytes() > self.backlog_limit {
            self.pacer.on_skipped(now);
            self.stats.frames_skipped += 1;
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
mod tests;
