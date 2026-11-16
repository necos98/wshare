//! The sound of a session, step by step, without threads or real devices:
//! captures while the sound is wanted, cuts it into chunks and sends them
//! as audio datagrams (see [`protocol::audio`]).
//!
//! - The capture is opened when the sound is wanted and closed when it is
//!   not (the viewer muted it, or the host's switch is off): nothing is
//!   captured or sent for nobody.
//! - Every opening is a new stream number, so the viewer starts over
//!   instead of mixing the old sound with the new.
//! - The default device is followed: when the user switches it, or the
//!   one captured fails, the capture is opened again on the default.
//! - Without a device the session goes on silent: one warning, then a new
//!   attempt every [`RETRY`], quietly, until one works.

use std::time::{Duration, Instant};

use anyhow::Result;
use pipeline::DatagramSink;
use pipeline::audio::Chunker;
use protocol::audio::packetize;

use super::super::events::EventSink;
use super::super::{HostEvent, HostWarning};

/// Time between two attempts to open a capture that failed.
pub(super) const RETRY: Duration = Duration::from_secs(2);
/// Time between two looks at which device is the default.
pub(super) const DEVICE_CHECK: Duration = Duration::from_secs(1);

/// A running capture.
pub(super) trait SoundCapture {
    /// Appends what was captured since the last call (interleaved samples
    /// in the wire format).
    fn read(&mut self, out: &mut Vec<i16>) -> Result<()>;
    /// The device captured.
    fn device(&self) -> &str;
}

/// The sound devices of this PC.
pub(super) trait SoundDevices {
    type Capture: SoundCapture;
    /// Starts capturing the default playback device.
    fn open(&self) -> Result<Self::Capture>;
    /// The default playback device now.
    fn default_id(&self) -> Result<String>;
}

/// A capture and the stream it feeds.
struct Live<C> {
    capture: C,
    chunker: Chunker,
    stream: u16,
    next_check: Instant,
}

/// See the module documentation.
pub(super) struct AudioSender<D: SoundDevices, S> {
    devices: D,
    sink: S,
    events: EventSink,
    live: Option<Live<D::Capture>>,
    next_stream: u16,
    /// When the next attempt to open may be made, after one failed.
    retry_at: Option<Instant>,
    /// The failure to open was reported and nothing worked since.
    warned: bool,
    /// Reused by every step.
    samples: Vec<i16>,
}

impl<D: SoundDevices, S: DatagramSink> AudioSender<D, S> {
    pub(super) fn new(devices: D, sink: S, events: EventSink) -> Self {
        Self {
            devices,
            sink,
            events,
            live: None,
            next_stream: 0,
            retry_at: None,
            warned: false,
            samples: Vec::new(),
        }
    }

    /// One turn, at `now`: captures and sends what came if the sound is
    /// `wanted`, closes the capture if not. Returns whether sound is being
    /// sent; fails only when the connection can no longer send.
    pub(super) fn step(&mut self, wanted: bool, now: Instant) -> Result<bool> {
        if !wanted {
            self.live = None;
            self.retry_at = None;
            return Ok(false);
        }
        if self.default_changed(now) {
            self.live = None;
        }
        if self.live.is_none() {
            self.open(now);
        }
        let Self {
            live: Some(live),
            sink,
            samples,
            ..
        } = self
        else {
            return Ok(false);
        };
        samples.clear();
        if live.capture.read(samples).is_err() {
            // The device went away or changed its format: the capture is
            // opened again on the default one, and a failure to do so is
            // what gets reported.
            self.live = None;
            self.open(now);
            return Ok(self.live.is_some());
        }
        let stream = live.stream;
        let max_datagram = sink.max_datagram_size();
        let mut sent = Ok(());
        live.chunker.push(samples, |position, chunk| {
            for datagram in packetize(stream, position, chunk, max_datagram) {
                if sent.is_ok() {
                    sent = sink.send(datagram);
                }
            }
        });
        sent.map(|()| true)
    }

    /// Opens the capture, unless the last attempt failed less than
    /// [`RETRY`] ago. The first failure of a row is reported.
    fn open(&mut self, now: Instant) {
        if self.retry_at.is_some_and(|at| now < at) {
            return;
        }
        match self.devices.open() {
            Ok(capture) => {
                self.live = Some(Live {
                    capture,
                    chunker: Chunker::new(),
                    stream: self.next_stream,
                    next_check: now + DEVICE_CHECK,
                });
                self.next_stream = self.next_stream.wrapping_add(1);
                self.retry_at = None;
                self.warned = false;
            }
            Err(error) => {
                self.retry_at = Some(now + RETRY);
                if !self.warned {
                    self.warned = true;
                    let warning = HostWarning::AudioUnavailable(format!("{error:#}"));
                    self.events.emit(HostEvent::Warning(warning));
                }
            }
        }
    }

    /// Whether, when a look is due, the default device is no longer the
    /// one captured (or there is none any more).
    fn default_changed(&mut self, now: Instant) -> bool {
        let Some(live) = &mut self.live else {
            return false;
        };
        if now < live.next_check {
            return false;
        }
        live.next_check = now + DEVICE_CHECK;
        self.devices
            .default_id()
            .map_or(true, |id| id != live.capture.device())
    }
}

#[cfg(test)]
mod tests;
