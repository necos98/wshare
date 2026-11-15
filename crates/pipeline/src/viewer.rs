//! Viewer loop: datagrams -> reassemble -> decode -> present.
//!
//! The app hands every datagram waiting for the render loop, as one batch,
//! to [`ViewerStreamer::on_datagrams`]; the returned actions tell it which
//! requests to send on the control channel. Every complete frame of a batch
//! is decoded (the decoder needs each one as a reference), but only the
//! newest picture is presented: after a hiccup the viewer jumps to the
//! present instead of replaying what it missed.
//!
//! A frame missing datagrams is first recovered ([`crate::recovery`]): the
//! viewer asks the host to resend them and holds the frames after it, so a
//! single lost datagram costs a round trip instead of a frozen picture
//! until the next keyframe. Only a frame that cannot be recovered in time
//! makes the viewer ask for a keyframe.

use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use protocol::recovery::Nack;
use protocol::video::{KeyframeRequester, ReassembledFrame, Reassembler, ReassemblyEvent};

use crate::recovery::{ASSUMED_RTT, LossRecovery};
use crate::traits::{FrameSink, VideoDecoder};

/// Unresolved frames kept: incomplete ones and the complete ones held
/// behind them while a resend is awaited. Covers the longest wait
/// ([`crate::recovery::MAX_RECOVERY_WAIT`]) at 100 fps; past it the oldest
/// frame is given up and a keyframe requested.
pub const MAX_PENDING_FRAMES: usize = 16;

/// Minimum time between two keyframe requests: long enough for the host to
/// answer one request (encode + send a keyframe) on a LAN, short enough that
/// a lost request is retried before the user notices a frozen picture.
pub const KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_millis(250);

/// A request for the host, to send on the control channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerAction {
    /// Send `ControlMessage::RequestKeyframe`.
    RequestKeyframe,
    /// Send `ControlMessage::Nack`: datagrams to send again.
    Nack(Nack),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewerStats {
    pub frames_decoded: u64,
    pub frames_presented: u64,
    /// Frames given up: their datagrams did not arrive, even resent.
    pub frames_lost: u64,
    /// Complete frames thrown away because they depend on a lost frame.
    pub frames_skipped: u64,
    /// Decoded pictures never shown because a newer one arrived in the same
    /// batch (the viewer was catching up).
    pub frames_superseded: u64,
    pub keyframes_requested: u64,
    /// Requests to resend datagrams.
    pub nacks_sent: u64,
    /// Frames decoded thanks to datagrams sent again.
    pub frames_recovered: u64,
}

pub struct ViewerStreamer<Dec, Sink>
where
    Dec: VideoDecoder,
    Sink: FrameSink<Frame = Dec::Frame>,
{
    reassembler: Reassembler,
    recovery: LossRecovery,
    keyframes: KeyframeRequester,
    /// When the latest datagram arrived.
    last_datagram: Option<Instant>,
    decoder: Dec,
    sink: Sink,
    stats: ViewerStats,
}

impl<Dec, Sink> ViewerStreamer<Dec, Sink>
where
    Dec: VideoDecoder,
    Sink: FrameSink<Frame = Dec::Frame>,
{
    /// Uses [`MAX_PENDING_FRAMES`], [`KEYFRAME_REQUEST_INTERVAL`] and
    /// [`ASSUMED_RTT`] until [`Self::set_rtt`].
    pub fn new(decoder: Dec, sink: Sink) -> Self {
        Self {
            reassembler: Reassembler::new(MAX_PENDING_FRAMES),
            recovery: LossRecovery::new(ASSUMED_RTT),
            keyframes: KeyframeRequester::new(KEYFRAME_REQUEST_INTERVAL),
            last_datagram: None,
            decoder,
            sink,
            stats: ViewerStats::default(),
        }
    }

    /// Feeds a batch of datagrams, received at `now`, in arrival order.
    /// Every frame they complete is decoded; only the last decoded picture
    /// is presented, the earlier ones count as
    /// [`ViewerStats::frames_superseded`]. Returns the requests to send:
    /// resends of missing datagrams, and a keyframe when a frame was given
    /// up (or we are still waiting for a keyframe) and the
    /// [`KeyframeRequester`] allows a new request at `now`.
    ///
    /// Malformed datagrams are ignored (a single corrupt datagram must not
    /// end the session); errors come only from the decoder or the sink.
    pub fn on_datagrams(
        &mut self,
        datagrams: impl IntoIterator<Item = Bytes>,
        now: Instant,
    ) -> Result<Vec<ViewerAction>> {
        let mut latest = None;
        for datagram in datagrams {
            self.last_datagram = Some(now);
            let Ok(events) = self.reassembler.push(datagram) else {
                continue;
            };
            self.handle(events, &mut latest)?;
        }
        self.conclude(now, latest)
    }

    /// Call when [`Self::next_wakeup`] is reached, and periodically (e.g.
    /// every 100 ms) even without datagrams, so a resend that does not come
    /// is given up and a still-missing keyframe is requested again.
    pub fn on_tick(&mut self, now: Instant) -> Result<Vec<ViewerAction>> {
        self.conclude(now, None)
    }

    /// When [`Self::on_tick`] next has recovery work (a deadline), if any.
    pub fn next_wakeup(&self) -> Option<Instant> {
        self.recovery.next_wakeup()
    }

    /// The network's round-trip time, which sets how long a resend is
    /// waited for.
    pub fn set_rtt(&mut self, rtt: Duration) {
        self.recovery.set_rtt(rtt);
    }

    /// Access to the sink for presentation concerns outside the stream
    /// (e.g. the window was resized and the last picture must be redrawn).
    pub fn sink_mut(&mut self) -> &mut Sink {
        &mut self.sink
    }

    pub fn stats(&self) -> &ViewerStats {
        &self.stats
    }

    /// Recovery decisions, then presentation of the newest picture, then a
    /// keyframe request if one is still needed.
    fn conclude(
        &mut self,
        now: Instant,
        mut latest: Option<Dec::Frame>,
    ) -> Result<Vec<ViewerAction>> {
        let mut actions = self.recover(now, &mut latest)?;
        if let Some(picture) = latest {
            self.sink.present(&picture)?;
            self.stats.frames_presented += 1;
        }
        if self.reassembler.awaiting_keyframe() && self.keyframes.should_request(now) {
            self.stats.keyframes_requested += 1;
            actions.push(ViewerAction::RequestKeyframe);
        }
        Ok(actions)
    }

    /// Asks for what is missing and gives up what is overdue.
    fn recover(
        &mut self,
        now: Instant,
        latest: &mut Option<Dec::Frame>,
    ) -> Result<Vec<ViewerAction>> {
        let gaps = self.reassembler.gaps();
        let last_datagram = self.last_datagram.unwrap_or(now);
        let step = self.recovery.poll(&gaps, last_datagram, now);
        if let Some(frame_id) = step.give_up_through {
            let events = self.reassembler.give_up(frame_id);
            self.handle(events, latest)?;
        }
        self.stats.nacks_sent += step.nacks.len() as u64;
        Ok(step.nacks.into_iter().map(ViewerAction::Nack).collect())
    }

    /// Decodes the frames among `events` (keeping the newest picture in
    /// `latest`) and counts the others.
    fn handle(
        &mut self,
        events: Vec<ReassemblyEvent>,
        latest: &mut Option<Dec::Frame>,
    ) -> Result<()> {
        for event in events {
            match event {
                ReassemblyEvent::Frame(frame) => {
                    if self.recovery.is_awaited(frame.frame_id) {
                        self.stats.frames_recovered += 1;
                    }
                    if let Some(picture) = self.decode(&frame)?
                        && latest.replace(picture).is_some()
                    {
                        self.stats.frames_superseded += 1;
                    }
                }
                ReassemblyEvent::Lost { .. } => self.stats.frames_lost += 1,
                ReassemblyEvent::Skipped { .. } => self.stats.frames_skipped += 1,
            }
        }
        Ok(())
    }

    fn decode(&mut self, frame: &ReassembledFrame) -> Result<Option<Dec::Frame>> {
        let picture = self.decoder.decode(&frame.data, frame.timestamp)?;
        if picture.is_some() {
            self.stats.frames_decoded += 1;
        }
        Ok(picture)
    }
}

#[cfg(test)]
mod tests;
