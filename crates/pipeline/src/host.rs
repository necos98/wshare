//! Host loop: capture -> encode -> packetize -> send.
//!
//! Runs on a dedicated (blocking) thread; the app calls [`HostStreamer::step`]
//! in a loop. Keyframe requests arriving from the network are forwarded with
//! [`HostStreamer::request_keyframe`].
//!
//! Under congestion frames are skipped *before* capture and encoding (see
//! [`crate::backlog`]): every frame that is encoded is also sent whole, so
//! the decoder's reference chain stays valid. The one exception is a frame
//! the transport's datagram limit shrinks under while it leaves: it is
//! given up and a keyframe starts the chain again.
//!
//! Every frame sent is kept for a moment in [`RecentFrames`], from which
//! the datagrams a viewer missed are sent again ([`crate::retransmit`]).
//! Large frames leave in a few bursts ([`crate::burst`]), and keyframe
//! requests are merged ([`crate::keyframe_gate`]), so one loss does not
//! snowball into a train of keyframes.
//!
//! Capture and encoder can be replaced while streaming (another monitor, a
//! new resolution, another frame rate): [`HostStreamer::into_continuation`]
//! and [`HostStreamer::resume`] keep the frame ids counting, so the viewer
//! sees one stream whose next frame is a keyframe of the new size.

use std::collections::VecDeque;
use std::time::Instant;

use anyhow::Result;
use bytes::Bytes;
use protocol::video::Packetizer;

use crate::backlog::send_backlog_limit;
use crate::burst::{BURST_GAP, burst_sizes};
use crate::keyframe_gate::KeyframeGate;
use crate::pacer::FramePacer;
use crate::retransmit::RecentFrames;
use crate::traits::{DatagramSink, DatagramTooLarge, EncodedFrame, FrameSource, VideoEncoder};

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
    /// Keyframe requests ignored because a keyframe was already on its way.
    pub keyframe_requests_merged: u64,
    /// Frames dropped part-sent because the datagram limit shrank under
    /// them; a keyframe replaces each.
    pub frames_too_large: u64,
    /// Resent datagrams dropped because they no longer fit the datagram
    /// limit (see [`RecentFrames::resend`]).
    pub resends_too_large: u64,
}

impl HostStats {
    /// These counts, with the resends answered from `recent` (by another
    /// thread) counted in.
    fn with_resends(&self, recent: &RecentFrames) -> Self {
        Self {
            resends_too_large: recent.resends_too_large(),
            ..self.clone()
        }
    }
}

/// What one stream hands to the next when capture and encoder start over
/// (see [`HostStreamer::into_continuation`] and [`HostStreamer::resume`]):
/// the sink, the frame numbering, the frames kept for resends, the clock
/// of the timestamps and the statistics.
pub struct StreamContinuation<D> {
    sink: D,
    packetizer: Packetizer,
    recent: RecentFrames,
    start: Instant,
    stats: HostStats,
}

impl<D> StreamContinuation<D> {
    /// What the streams so far did, e.g. to report them when no new
    /// stream can be started.
    pub fn stats(&self) -> HostStats {
        self.stats.with_resends(&self.recent)
    }
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
    recent: RecentFrames,
    keyframes: KeyframeGate,
    start: Instant,
    encoded: VecDeque<EncodedFrame>,
    /// Frames submitted to the current encoder and not sent yet.
    in_flight: u64,
    fps: u32,
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
    /// [`send_backlog_limit`]). Every frame sent is recorded in `recent`,
    /// which the caller shares with whatever answers the viewer's resend
    /// requests. Panics if `fps` is 0.
    pub fn new(
        source: S,
        encoder: E,
        sink: D,
        fps: u32,
        bitrate_bps: u32,
        recent: RecentFrames,
    ) -> Self {
        let continuation = StreamContinuation {
            sink,
            packetizer: Packetizer::new(),
            recent,
            start: Instant::now(),
            stats: HostStats::default(),
        };
        Self::resume(continuation, source, encoder, fps, bitrate_bps)
    }

    /// Goes on with the stream `continuation` left off, from a new source
    /// and encoder (e.g. another monitor, or the same one in a new
    /// resolution) at `fps` and `bitrate_bps`. Frame ids, timestamps and
    /// statistics carry on, so a viewer follows without starting over; the
    /// first `step` sends a picture at once, like after [`Self::new`]. The
    /// new encoder must begin with a keyframe (every encoder does).
    /// Panics if `fps` is 0.
    pub fn resume(
        continuation: StreamContinuation<D>,
        source: S,
        encoder: E,
        fps: u32,
        bitrate_bps: u32,
    ) -> Self {
        let StreamContinuation {
            sink,
            packetizer,
            recent,
            start,
            stats,
        } = continuation;
        let mut pacer = FramePacer::new(fps);
        pacer.mark_dirty();
        Self {
            source,
            encoder,
            sink,
            pacer,
            packetizer,
            recent,
            keyframes: KeyframeGate::new(),
            start,
            encoded: VecDeque::new(),
            in_flight: 0,
            fps,
            backlog_limit: send_backlog_limit(bitrate_bps, fps),
            stats,
        }
    }

    /// Ends this source and encoder (both are dropped here, so the next
    /// ones can open the same devices) and keeps what the stream that
    /// follows needs, for [`Self::resume`]. Frames the old encoder still
    /// holds are thrown away unsent: they would only precede the new
    /// stream's keyframe.
    pub fn into_continuation(self) -> StreamContinuation<D> {
        StreamContinuation {
            sink: self.sink,
            packetizer: self.packetizer,
            recent: self.recent,
            start: self.start,
            stats: self.stats,
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
    /// dirty, so a keyframe is produced even if the screen is static —
    /// unless a keyframe was forced or sent moments ago (see
    /// [`KeyframeGate`]): the request is then merged into that one and
    /// counted in [`HostStats::keyframe_requests_merged`].
    pub fn request_keyframe(&mut self) -> Result<()> {
        if !self.keyframes.accept(Instant::now()) {
            self.stats.keyframe_requests_merged += 1;
            return Ok(());
        }
        self.force_keyframe()
    }

    /// Retargets the encoder (see [`crate::bitrate`]) and resizes the send
    /// backlog allowed to match, so the queue stays a few frames long at
    /// the new rate. If the encoder refuses, nothing changes.
    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.encoder.set_bitrate(bitrate_bps)?;
        self.backlog_limit = send_backlog_limit(bitrate_bps, self.fps);
        Ok(())
    }

    /// What the stream did so far, resends included.
    pub fn stats(&self) -> HostStats {
        self.stats.with_resends(&self.recent)
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

    /// Packetizes `frame`, records it for resends (before sending, so a
    /// request can never outrun it) and sends it, in bursts if large.
    fn send_frame(&mut self, frame: &EncodedFrame) -> Result<()> {
        let max_size = self.sink.max_datagram_size();
        let frame_id = self.packetizer.next_frame_id();
        let datagrams =
            self.packetizer
                .packetize(&frame.data, frame.keyframe, frame.timestamp, max_size)?;
        self.recent.record(frame_id, &datagrams, Instant::now());
        self.in_flight = self.in_flight.saturating_sub(1);
        match self.send_in_bursts(datagrams) {
            Ok(()) => {
                self.on_frame_sent(frame.keyframe);
                Ok(())
            }
            Err(error) if error.is::<DatagramTooLarge>() => self.on_frame_too_large(),
            Err(error) => Err(error),
        }
    }

    fn on_frame_sent(&mut self, keyframe: bool) {
        self.stats.frames_sent += 1;
        if keyframe {
            self.stats.keyframes_sent += 1;
            self.keyframes.on_keyframe_sent(Instant::now());
        }
    }

    /// The datagram limit shrank while a frame was leaving (e.g. between
    /// two bursts): its remaining datagrams no longer fit, and the frame
    /// cannot be packetized again under the same id. It is given up, and
    /// so is every frame that refers to it, until the keyframe forced here,
    /// packetized for the new limit. The viewer cannot decode on without
    /// it whatever the [`KeyframeGate`] says, so the gate is not asked; the
    /// requests the viewer sends for it are merged into this keyframe.
    fn on_frame_too_large(&mut self) -> Result<()> {
        self.stats.frames_too_large += 1;
        self.keyframes.on_keyframe_forced(Instant::now());
        self.force_keyframe()
    }

    /// Makes the next encoded frame a keyframe and marks the pacer dirty,
    /// so one is produced even if the screen is static.
    fn force_keyframe(&mut self) -> Result<()> {
        self.encoder.request_keyframe()?;
        self.pacer.mark_dirty();
        Ok(())
    }

    /// Sends `datagrams` in the bursts [`burst_sizes`] plans, pausing
    /// [`BURST_GAP`] between two (an ordinary frame is one burst: no pause).
    fn send_in_bursts(&mut self, datagrams: Vec<Bytes>) -> Result<()> {
        let lengths: Vec<usize> = datagrams.iter().map(Bytes::len).collect();
        let mut datagrams = datagrams.into_iter();
        for (burst, size) in burst_sizes(&lengths).into_iter().enumerate() {
            if burst > 0 {
                std::thread::sleep(BURST_GAP);
            }
            for datagram in datagrams.by_ref().take(size) {
                let len = datagram.len() as u64;
                self.sink.send(datagram)?;
                self.stats.datagrams_sent += 1;
                self.stats.bytes_sent += len;
            }
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
        self.in_flight += 1;
        self.flush_in_flight()
    }

    /// Waits for the frame just submitted and sends it right away. Without
    /// this, a frame finishing while we block in `wait_for_change` would sit
    /// in the encoder for up to one interval. Bounded by `MAX_FLUSH_EVENTS`
    /// so an encoder that buffers frames internally cannot stall the loop.
    fn flush_in_flight(&mut self) -> Result<()> {
        for _ in 0..MAX_FLUSH_EVENTS {
            if self.in_flight == 0 {
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
