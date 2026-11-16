//! The viewer's jitter buffer: absorbs the uneven arrival of the audio
//! datagrams and hands the sound card a steady stream.
//!
//! - **Delay**: playback starts once [`JitterBuffer::target`] of sound is
//!   buffered. The target follows the measured jitter (RFC 3550 estimate,
//!   one sample per 10 ms chunk) between [`MIN_DELAY`] and [`MAX_DELAY`].
//! - **Losses**: a frame that never came is played as silence; one that
//!   comes after its turn is dropped. Datagrams carry their position, so
//!   order does not matter while they are on time.
//! - **Underrun**: with nothing left to play, the buffer goes silent and
//!   fills up to the target again. That is also what happens when the host
//!   goes quiet: Windows delivers no sound while nothing plays.
//! - **Drift**: the host's and the viewer's sound cards never run at quite
//!   the same rate (tens of ppm). The buffered amount, smoothed, is kept
//!   within [`DRIFT_TOLERANCE`] of the target by dropping or repeating one
//!   frame per pull; a burst far above the target (after a network stall)
//!   is skipped at once so the delay does not stay high.
//! - **Streams**: a new stream number (the host restarted its capture)
//!   starts over; so does a position absurdly far from the one playing.
//!
//! Pure: time comes in with every call, nothing here blocks.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use protocol::audio::{CHANNELS, CHUNK_FRAMES, SAMPLE_RATE};

/// Shortest delay aimed at: the chunk being collected on the host, the
/// datagrams' way and the viewer's own pull period, with room to spare.
pub const MIN_DELAY: Duration = Duration::from_millis(40);
/// Longest delay aimed at, however jittery the network: past it the sound
/// lags the picture noticeably.
pub const MAX_DELAY: Duration = Duration::from_millis(100);
/// Distance of the smoothed level from the target that drift corrections
/// tolerate: more than a chunk, so the level's natural swing (a chunk
/// arrives, the card pulls) never triggers them.
pub const DRIFT_TOLERANCE: Duration = Duration::from_millis(12);
/// Excess over the target that is dropped at once instead of corrected
/// frame by frame.
const CATCH_UP_EXCESS: Duration = Duration::from_millis(60);
/// Longest span of sound held: a position further ahead than this starts
/// the buffer over instead of filling it with silence.
const MAX_SPAN: Duration = Duration::from_secs(1);
/// Weight of each new sample in the jitter estimate (RFC 3550: 1/16).
const JITTER_GAIN: f64 = 1.0 / 16.0;
/// Weight of each pull in the smoothed level: about 32 pulls, a third of
/// a second at the usual 10 ms period.
const LEVEL_GAIN: f64 = 1.0 / 32.0;
/// Jitter the delay makes room for, in multiples of the estimate (the
/// estimate is a mean deviation; spikes go well beyond it).
const JITTER_MARGIN: f64 = 4.0;

const CHUNK_SECONDS: f64 = CHUNK_FRAMES as f64 / SAMPLE_RATE as f64;

/// What happened to the sound so far, for statistics and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JitterStats {
    /// Frames that arrived after their turn.
    pub late_frames: u64,
    /// Frames played as silence because they never came.
    pub concealed_frames: u64,
    /// Times the buffer ran dry while playing.
    pub underruns: u64,
    /// Frames skipped to keep the delay down (drift, bursts).
    pub dropped_frames: u64,
    /// Frames repeated to keep the delay up (drift).
    pub inserted_frames: u64,
    /// Times the buffer started over on a position far from the one played.
    pub resyncs: u64,
}

/// A one-frame correction of the drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Correction {
    None,
    Drop,
    Repeat,
}

/// See the module documentation.
#[derive(Debug)]
pub struct JitterBuffer {
    stream: Option<u16>,
    /// Position of the frame at the front, the next one to play.
    start: u64,
    /// Interleaved samples from `start` on.
    samples: VecDeque<i16>,
    /// Per frame of `samples`: whether it arrived. The last frame always
    /// did (the buffer only grows by what arrives).
    present: VecDeque<bool>,
    playing: bool,
    /// Buffered frames, smoothed over the pulls.
    level: f64,
    /// Mean deviation of the transit time, in seconds.
    jitter: f64,
    /// Chunk and transit time of the last jitter sample.
    last_transit: Option<(u64, f64)>,
    epoch: Instant,
    target_frames: usize,
    stats: JitterStats,
}

impl JitterBuffer {
    pub fn new(now: Instant) -> Self {
        Self {
            stream: None,
            start: 0,
            samples: VecDeque::new(),
            present: VecDeque::new(),
            playing: false,
            level: 0.0,
            jitter: 0.0,
            last_transit: None,
            epoch: now,
            target_frames: frames_of(MIN_DELAY),
            stats: JitterStats::default(),
        }
    }

    /// Forgets everything buffered: what arrives next is a new stream (e.g.
    /// after reconnecting). Statistics are kept.
    pub fn reset(&mut self) {
        self.stream = None;
        self.clear_at(0);
        self.jitter = 0.0;
        self.target_frames = frames_of(MIN_DELAY);
    }

    /// The delay aimed at now.
    pub fn target(&self) -> Duration {
        duration_of(self.target_frames)
    }

    /// The sound buffered now, from the next frame to play to the newest.
    pub fn buffered(&self) -> Duration {
        duration_of(self.present.len())
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    /// Takes the interleaved `samples` that start at `position` of `stream`,
    /// received at `now`. A trailing partial frame is ignored.
    pub fn push(&mut self, stream: u16, position: u64, samples: &[i16], now: Instant) {
        let frames = samples.len() / CHANNELS;
        if frames == 0 {
            return;
        }
        let end = position + frames as u64;
        if self.stream != Some(stream) {
            self.stream = Some(stream);
            self.clear_at(position);
        } else if end <= self.start {
            if end + frames_of(MAX_SPAN) as u64 <= self.start {
                // Far behind: not a late datagram but another stream.
                self.resync(position);
            } else {
                // Too late to play, but it tells how late the network runs.
                self.stats.late_frames += frames as u64;
                self.sample_jitter(position, now);
                return;
            }
        } else if !self.playing && self.present.is_empty() {
            // Nothing to play yet: start at the first sound that comes.
            self.clear_at(position);
        } else if end > self.start + frames_of(MAX_SPAN) as u64 {
            self.resync(position);
        }
        self.store(position, &samples[..frames * CHANNELS]);
        self.sample_jitter(position, now);
        if !self.playing && self.present.len() >= self.target_frames {
            self.playing = true;
            self.level = self.present.len() as f64;
        }
    }

    /// Fills `out` (interleaved) with the next frames to play: silence
    /// while buffering or where sound is missing.
    pub fn pull(&mut self, out: &mut [i16]) {
        let wanted = out.len() / CHANNELS;
        if !self.playing || wanted == 0 {
            out.fill(0);
            return;
        }
        self.catch_up();
        let mut written = 0;
        match self.correction(wanted) {
            Correction::None => {}
            Correction::Drop => {
                self.discard(1);
                self.stats.dropped_frames += 1;
            }
            Correction::Repeat => {
                // The front frame is played twice: once here, once below.
                for (channel, sample) in out[..CHANNELS].iter_mut().enumerate() {
                    *sample = self.samples[channel];
                }
                written = 1;
                self.stats.inserted_frames += 1;
            }
        }
        let rest = &mut out[written * CHANNELS..wanted * CHANNELS];
        let available = (rest.len() / CHANNELS).min(self.present.len());
        let (played, missing) = rest.split_at_mut(available * CHANNELS);
        // Frames that never came are stored as silence: only counted here.
        for (slot, sample) in played
            .iter_mut()
            .zip(self.samples.drain(..available * CHANNELS))
        {
            *slot = sample;
        }
        let concealed = self.present.drain(..available).filter(|came| !came).count();
        self.stats.concealed_frames += concealed as u64;
        self.start += available as u64;
        missing.fill(0);
        out[wanted * CHANNELS..].fill(0);
        if self.present.is_empty() {
            self.underrun();
        }
    }

    /// Writes `samples` at `position`, which ends after `start`: the part
    /// before `start` is late and dropped, frames between the buffered ones
    /// and `position` are marked missing.
    fn store(&mut self, position: u64, samples: &[i16]) {
        let skipped = self.start.saturating_sub(position) as usize;
        if skipped > 0 {
            self.stats.late_frames += skipped as u64;
        }
        let offset = (position + skipped as u64 - self.start) as usize;
        let samples = &samples[skipped * CHANNELS..];
        let end = offset + samples.len() / CHANNELS;
        if end > self.present.len() {
            self.present.resize(end, false);
            self.samples.resize(end * CHANNELS, 0);
        }
        self.present
            .range_mut(offset..end)
            .for_each(|came| *came = true);
        let slots = self.samples.range_mut(offset * CHANNELS..end * CHANNELS);
        for (slot, sample) in slots.zip(samples) {
            *slot = *sample;
        }
    }

    /// Updates the smoothed level and decides the drift correction of this
    /// pull. Repeating needs a frame to repeat and more than the pull
    /// buffered, so it never brings an underrun closer.
    fn correction(&mut self, wanted: usize) -> Correction {
        let level = self.present.len() as f64;
        self.level += (level - self.level) * LEVEL_GAIN;
        let target = self.target_frames as f64;
        let tolerance = frames_of(DRIFT_TOLERANCE) as f64;
        if self.level > target + tolerance && self.present.len() > wanted {
            Correction::Drop
        } else if self.level < target - tolerance && self.present.len() > wanted {
            Correction::Repeat
        } else {
            Correction::None
        }
    }

    /// Drops at once what is far above the target (a burst after a stall),
    /// keeping the target buffered.
    fn catch_up(&mut self) {
        let excess = self.present.len().saturating_sub(self.target_frames);
        if excess <= frames_of(CATCH_UP_EXCESS) {
            return;
        }
        self.discard(excess);
        self.level = self.present.len() as f64;
        self.stats.dropped_frames += excess as u64;
    }

    /// Skips the `frames` at the front, which must be buffered.
    fn discard(&mut self, frames: usize) {
        self.present.drain(..frames);
        self.samples.drain(..frames * CHANNELS);
        self.start += frames as u64;
    }

    /// One jitter sample per chunk: the transit time of its first datagram,
    /// compared with the previous chunk's.
    fn sample_jitter(&mut self, position: u64, now: Instant) {
        let chunk = position / CHUNK_FRAMES as u64;
        if self.last_transit.is_some_and(|(last, _)| last == chunk) {
            return;
        }
        let arrival = now.saturating_duration_since(self.epoch).as_secs_f64();
        let transit = arrival - chunk as f64 * CHUNK_SECONDS;
        if let Some((_, last)) = self.last_transit {
            self.jitter += ((transit - last).abs() - self.jitter) * JITTER_GAIN;
        }
        self.last_transit = Some((chunk, transit));
        let wanted = CHUNK_SECONDS + JITTER_MARGIN * self.jitter;
        let wanted = Duration::from_secs_f64(wanted).clamp(MIN_DELAY, MAX_DELAY);
        self.target_frames = frames_of(wanted);
    }

    /// The buffer ran dry: silence until the target is buffered again. The
    /// next sound may come after a pause (the host went quiet), so its
    /// transit time is not compared with the last one.
    fn underrun(&mut self) {
        self.playing = false;
        self.last_transit = None;
        self.stats.underruns += 1;
    }

    fn resync(&mut self, position: u64) {
        self.clear_at(position);
        self.stats.resyncs += 1;
    }

    /// Empties the buffer; the next frame to play is `position`.
    fn clear_at(&mut self, position: u64) {
        self.samples.clear();
        self.present.clear();
        self.start = position;
        self.playing = false;
        self.last_transit = None;
    }
}

fn frames_of(duration: Duration) -> usize {
    (duration.as_secs_f64() * f64::from(SAMPLE_RATE)).round() as usize
}

fn duration_of(frames: usize) -> Duration {
    Duration::from_secs_f64(frames as f64 / f64::from(SAMPLE_RATE))
}

#[cfg(test)]
mod tests;
