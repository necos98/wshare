//! The host's sound on this PC.
//!
//! The network task hands every audio datagram to [`AudioIn`], which puts
//! it in the jitter buffer (see `pipeline::audio`); a thread of its own
//! ([`AudioPlayer`]) keeps the default playback device fed from it, about
//! [`DEVICE_QUEUE`] ahead. The video never waits for any of this: audio
//! datagrams are taken out of the stream before the render queue.
//!
//! Without a playback device the session goes on silent (a warning, then
//! a new attempt every [`RETRY`]); a device that changes or goes away is
//! followed like on the host.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use audio::{ComApartment, DefaultDevice, Playback};
use pipeline::audio::JitterBuffer;
use protocol::audio::{AudioPacket, SAMPLE_RATE};

use crate::realtime::{MmcssRegistration, TimerResolution};

/// Sound kept queued in the device: enough to ride out a late wake-up of
/// the thread, little enough to add no noticeable delay.
const DEVICE_QUEUE: Duration = Duration::from_millis(20);
/// Time between two top-ups of the device queue.
const POLL: Duration = Duration::from_millis(5);
/// Time between two attempts to open a device after one failed.
const RETRY: Duration = Duration::from_secs(2);
/// Time between two looks at which device is the default.
const DEVICE_CHECK: Duration = Duration::from_secs(1);

/// The jitter buffer and the scratch space to decode into.
struct Incoming {
    jitter: JitterBuffer,
    samples: Vec<i16>,
}

/// Where the network task puts the host's sound. Cheap to clone.
#[derive(Clone)]
pub struct AudioIn(Arc<Mutex<Incoming>>);

impl AudioIn {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Incoming {
            jitter: JitterBuffer::new(Instant::now()),
            samples: Vec::new(),
        })))
    }

    /// Takes one audio datagram. One that does not parse is dropped, as if
    /// it had been lost.
    pub fn push(&self, datagram: &[u8]) {
        let Ok(packet) = AudioPacket::parse(datagram) else {
            return;
        };
        let mut incoming = self.lock();
        let Incoming { jitter, samples } = &mut *incoming;
        samples.clear();
        packet.samples_into(samples);
        jitter.push(packet.stream, packet.position, samples, Instant::now());
    }

    /// Forgets the sound buffered: a new connection numbers its own.
    pub fn reset(&self) {
        self.lock().jitter.reset();
    }

    /// The next frames to play, into `out` (interleaved).
    fn pull(&self, out: &mut [i16]) {
        self.lock().jitter.pull(out);
    }

    /// A panic elsewhere must not silence the sound for good: the buffer
    /// stays consistent between calls.
    fn lock(&self) -> MutexGuard<'_, Incoming> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for AudioIn {
    fn default() -> Self {
        Self::new()
    }
}

/// Owns the playback thread; dropping it stops the sound.
pub struct AudioPlayer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AudioPlayer {
    /// Starts playing what arrives in `input`.
    pub fn start(input: AudioIn) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("wshare-audio".into())
                .spawn(move || play(&input, &stop))
                .context("spawning the audio thread")?
        };
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            eprintln!("warning: the audio thread panicked");
        }
    }
}

/// The playback device and when to look at it again.
struct Output {
    playback: Playback,
    next_check: Instant,
}

/// Body of the playback thread.
fn play(input: &AudioIn, stop: &AtomicBool) {
    let _com = ComApartment::enter();
    let _timer = TimerResolution::raise();
    let _mmcss = MmcssRegistration::register();
    let mut output: Option<Output> = None;
    let mut retry_at = None;
    let mut warned = false;
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if output.as_mut().is_some_and(|out| default_changed(out, now)) {
            output = None;
        }
        if output.is_none() && retry_at.is_none_or(|at| now >= at) {
            match open(input, now) {
                Ok(opened) => {
                    output = Some(opened);
                    retry_at = None;
                    warned = false;
                }
                Err(e) => {
                    retry_at = Some(now + RETRY);
                    if !warned {
                        warned = true;
                        eprintln!("warning: the host's sound cannot be played: {e:#}");
                    }
                }
            }
        }
        if let Some(out) = &mut output
            && top_up(&mut out.playback, input).is_err()
        {
            // The device went away or changed its format: opened again at
            // the next turn, on the default one; a failure to do so is
            // what gets reported.
            output = None;
        }
        std::thread::sleep(POLL);
    }
}

/// Opens the default device. What was buffered meanwhile is stale: the
/// sound starts over with the next datagram.
fn open(input: &AudioIn, now: Instant) -> Result<Output> {
    let playback = Playback::open(&DefaultDevice::new()?)?;
    input.reset();
    Ok(Output {
        playback,
        next_check: now + DEVICE_CHECK,
    })
}

/// Whether, when a look is due, the default device is no longer the one
/// played on (or there is none any more).
fn default_changed(output: &mut Output, now: Instant) -> bool {
    if now < output.next_check {
        return false;
    }
    output.next_check = now + DEVICE_CHECK;
    let id = DefaultDevice::new().and_then(|devices| devices.id());
    id.map_or(true, |id| id != output.playback.device())
}

/// Fills the device queue up to [`DEVICE_QUEUE`] from the jitter buffer.
fn top_up(playback: &mut Playback, input: &AudioIn) -> Result<()> {
    let frames = frames_to_write(playback.queued_frames()?, playback.free_frames()?);
    playback.write(frames, |out| input.pull(out))
}

/// Frames to write to bring the device queue (`queued` now, room for
/// `free` more) up to [`DEVICE_QUEUE`].
fn frames_to_write(queued: u32, free: u32) -> u32 {
    let wanted = (DEVICE_QUEUE.as_secs_f64() * f64::from(SAMPLE_RATE)) as u32;
    wanted.saturating_sub(queued).min(free)
}

#[cfg(test)]
mod tests {
    use protocol::audio::{CHUNK_FRAMES, CHUNK_SAMPLES, packetize};

    use super::*;

    #[test]
    fn the_device_queue_is_topped_up_to_twenty_milliseconds() {
        assert_eq!(frames_to_write(0, 4_800), 960);
        assert_eq!(frames_to_write(500, 4_300), 460);
        assert_eq!(frames_to_write(960, 3_840), 0);
        assert_eq!(frames_to_write(2_000, 2_800), 0);
        assert_eq!(frames_to_write(0, 100), 100, "never more than fits");
    }

    #[test]
    fn datagrams_reach_the_jitter_buffer_and_garbage_does_not() {
        let input = AudioIn::new();
        input.push(&[0xA1, 0, 0]);
        input.push(&[]);
        for chunk in 0..8u64 {
            let position = chunk * CHUNK_FRAMES as u64;
            for datagram in packetize(0, position, &vec![7; CHUNK_SAMPLES], 1200) {
                input.push(&datagram);
            }
        }
        let mut out = vec![0; 8];
        input.pull(&mut out);
        assert_eq!(out, [7; 8]);
        input.reset();
        input.pull(&mut out);
        assert_eq!(out, [0; 8], "silent after a reset");
    }
}
