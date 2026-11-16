//! Tests of [`super::JitterBuffer`], driven by a simulated clock.

use super::*;

const CHUNK: usize = CHUNK_FRAMES;
const TICK: Duration = Duration::from_millis(10);

/// A chunk whose every sample is `value`.
fn chunk(value: i16) -> Vec<i16> {
    vec![value; CHUNK * CHANNELS]
}

/// Samples that say which frame they are: frame `n` holds `n` (wrapping).
fn numbered(position: u64, frames: usize) -> Vec<i16> {
    (0..frames as u64)
        .flat_map(|n| [(position + n) as i16; CHANNELS])
        .collect()
}

fn pull(buffer: &mut JitterBuffer, frames: usize) -> Vec<i16> {
    let mut out = vec![i16::MAX; frames * CHANNELS];
    buffer.pull(&mut out);
    out
}

/// Pushes one chunk per tick from `first` until playback starts; returns
/// the position after the last chunk pushed and the time.
fn fill_to_target(buffer: &mut JitterBuffer, first: u64, mut now: Instant) -> (u64, Instant) {
    let mut position = first;
    while !buffer.is_playing() {
        buffer.push(0, position, &numbered(position, CHUNK), now);
        position += CHUNK as u64;
        now += TICK;
    }
    (position, now)
}

#[test]
fn it_is_silent_until_the_target_is_buffered_then_plays_in_order() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    assert_eq!(buffer.target(), MIN_DELAY);
    assert!(pull(&mut buffer, CHUNK).iter().all(|s| *s == 0));
    let (_, _) = fill_to_target(&mut buffer, 0, start);
    assert!(buffer.buffered() >= MIN_DELAY);
    let out = pull(&mut buffer, CHUNK);
    assert_eq!(out, numbered(0, CHUNK));
    let out = pull(&mut buffer, 10);
    assert_eq!(out, numbered(CHUNK as u64, 10));
}

#[test]
fn playback_starts_at_the_first_sound_received() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    // The host's position is far from zero (a session running for a while).
    let (_, _) = fill_to_target(&mut buffer, 1_000 * CHUNK as u64, start);
    assert_eq!(pull(&mut buffer, 3), numbered(1_000 * CHUNK as u64, 3));
}

#[test]
fn a_lost_datagram_is_played_as_silence_and_the_rest_goes_on() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    let half = CHUNK / 2;
    // Chunk 1 lost its second half.
    buffer.push(0, 0, &chunk(1), start);
    buffer.push(0, CHUNK as u64, &chunk(2)[..half * CHANNELS], start);
    for n in 2..6 {
        buffer.push(0, n * CHUNK as u64, &chunk(n as i16 + 1), start);
    }
    assert!(buffer.is_playing());
    assert!(pull(&mut buffer, CHUNK).iter().all(|s| *s == 1));
    let second = pull(&mut buffer, CHUNK);
    assert!(second[..half * CHANNELS].iter().all(|s| *s == 2));
    assert!(second[half * CHANNELS..].iter().all(|s| *s == 0));
    assert!(pull(&mut buffer, CHUNK).iter().all(|s| *s == 3));
    assert_eq!(buffer.stats().concealed_frames, half as u64);
    assert_eq!(buffer.stats().underruns, 0);
}

#[test]
fn a_datagram_after_its_turn_is_dropped_and_one_out_of_order_fits_in() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    buffer.push(0, 0, &chunk(1), start);
    // Chunk 1 is overtaken by chunks 2..5.
    for n in 2..6 {
        buffer.push(0, n * CHUNK as u64, &chunk(n as i16 + 1), start);
    }
    buffer.push(0, CHUNK as u64, &chunk(2), start);
    pull(&mut buffer, CHUNK);
    assert!(pull(&mut buffer, CHUNK).iter().all(|s| *s == 2), "in time");
    // Chunk 0 again, long played.
    buffer.push(0, 0, &chunk(9), start);
    assert_eq!(buffer.stats().late_frames, CHUNK as u64);
    assert!(pull(&mut buffer, CHUNK).iter().all(|s| *s == 3));
    // Partly late: the part still to come is kept.
    buffer.push(0, 3 * CHUNK as u64 - 10, &numbered(0, 20), start);
    assert_eq!(buffer.stats().late_frames, CHUNK as u64 + 10);
    let out = pull(&mut buffer, 10);
    assert_eq!(out, numbered(10, 10));
}

#[test]
fn running_dry_goes_silent_and_waits_for_the_target_again() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    let (position, now) = fill_to_target(&mut buffer, 0, start);
    let buffered = position as usize;
    let out = pull(&mut buffer, buffered + CHUNK);
    assert_eq!(&out[..buffered * CHANNELS], numbered(0, buffered));
    assert!(out[buffered * CHANNELS..].iter().all(|s| *s == 0));
    assert!(!buffer.is_playing());
    assert_eq!(buffer.stats().underruns, 1);
    // The host was quiet for a while; its next sound continues the count.
    let later = now + Duration::from_secs(5);
    buffer.push(0, position, &numbered(position, CHUNK), later);
    assert!(
        pull(&mut buffer, CHUNK).iter().all(|s| *s == 0),
        "buffering"
    );
    fill_to_target(&mut buffer, position + CHUNK as u64, later + TICK);
    assert_eq!(pull(&mut buffer, 2), numbered(position, 2));
    assert_eq!(buffer.target(), MIN_DELAY, "a pause is not jitter");
}

#[test]
fn a_new_stream_starts_over() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    fill_to_target(&mut buffer, 50 * CHUNK as u64, start);
    buffer.push(1, 0, &chunk(7), start);
    assert!(!buffer.is_playing());
    assert_eq!(buffer.buffered(), duration_of(CHUNK));
    for n in 1..4 {
        buffer.push(1, n * CHUNK as u64, &chunk(7), start);
    }
    assert!(pull(&mut buffer, CHUNK).iter().all(|s| *s == 7));
    assert_eq!(buffer.stats().resyncs, 0);
}

#[test]
fn a_position_far_from_the_one_playing_starts_over() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    let (position, _) = fill_to_target(&mut buffer, 0, start);
    let far = position + 10 * SAMPLE_RATE as u64;
    buffer.push(0, far, &chunk(5), start);
    assert_eq!(buffer.stats().resyncs, 1);
    assert!(!buffer.is_playing());
    // Far behind too (a peer that numbered from zero again).
    let mut buffer = JitterBuffer::new(start);
    fill_to_target(&mut buffer, 100 * SAMPLE_RATE as u64, start);
    buffer.push(0, 0, &chunk(5), start);
    assert_eq!(buffer.stats().resyncs, 1);
    assert_eq!(buffer.stats().late_frames, 0);
}

#[test]
fn a_reset_forgets_the_stream() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    fill_to_target(&mut buffer, 10_000, start);
    buffer.reset();
    assert!(!buffer.is_playing());
    assert_eq!(buffer.buffered(), Duration::ZERO);
    // The same stream number from zero: accepted as new.
    fill_to_target(&mut buffer, 0, start);
    assert_eq!(pull(&mut buffer, 1), numbered(0, 1));
}

#[test]
fn a_burst_after_a_stall_is_skipped_down_to_the_target() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    let (mut position, now) = fill_to_target(&mut buffer, 0, start);
    // 300 ms arrive at once.
    for _ in 0..30 {
        buffer.push(0, position, &numbered(position, CHUNK), now);
        position += CHUNK as u64;
    }
    pull(&mut buffer, CHUNK);
    let left = buffer.buffered();
    assert!(left <= buffer.target(), "{left:?}");
    assert!(buffer.stats().dropped_frames > 0);
    // What plays next is the newest sound, in order.
    let next = pull(&mut buffer, 2);
    assert_eq!(next[0] as u64 + 1, next[CHANNELS] as u64);
}

#[test]
fn a_steady_network_keeps_the_shortest_delay() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    let mut now = start;
    for n in 0..500u64 {
        buffer.push(0, n * CHUNK as u64, &chunk(1), now);
        // Both datagrams of a chunk arrive together: no jitter.
        buffer.push(0, n * CHUNK as u64 + 240, &chunk(1)[..240 * CHANNELS], now);
        pull(&mut buffer, CHUNK);
        now += TICK;
    }
    assert_eq!(buffer.target(), MIN_DELAY);
}

#[test]
fn a_jittery_network_raises_the_delay_within_its_bounds() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    for n in 0..500u64 {
        // Every other chunk is 10 ms late.
        let delay = if n % 2 == 0 { 0 } else { 10 };
        let now = start + TICK * n as u32 + Duration::from_millis(delay);
        buffer.push(0, n * CHUNK as u64, &chunk(1), now);
        pull(&mut buffer, CHUNK);
    }
    let target = buffer.target();
    assert!(target > MIN_DELAY && target <= MAX_DELAY, "{target:?}");
    for n in 500..1000u64 {
        let delay = if n % 2 == 0 { 0 } else { 500 };
        let now = start + TICK * n as u32 + Duration::from_millis(delay);
        buffer.push(0, n * CHUNK as u64, &chunk(1), now);
    }
    assert_eq!(buffer.target(), MAX_DELAY);
}

/// Thirty minutes of a host whose sound card runs `ppm` faster (or, when
/// negative, slower) than the viewer's: the delay must stay near the target
/// all along, without ever running dry.
fn simulate_drift(ppm: f64) {
    const MINUTES: u32 = 30;
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    let mut produced = 0.0f64;
    let mut position = 0u64;
    let mut out = vec![0; CHUNK * CHANNELS];
    let samples = chunk(1);
    let tolerance = DRIFT_TOLERANCE + duration_of(3 * CHUNK);
    for tick in 0..MINUTES * 60 * 100 {
        let now = start + TICK * tick;
        produced += CHUNK as f64 * (1.0 + ppm / 1e6);
        while produced >= (position + CHUNK as u64) as f64 {
            buffer.push(0, position, &samples, now);
            position += CHUNK as u64;
        }
        buffer.pull(&mut out);
        if tick > 100 * 60 {
            let buffered = buffer.buffered();
            let target = buffer.target();
            assert!(
                buffered + tolerance >= target && buffered <= target + tolerance,
                "{buffered:?} buffered for {target:?} after {tick} ticks"
            );
        }
    }
    let stats = buffer.stats();
    assert_eq!(stats.underruns, 0);
    assert_eq!(stats.concealed_frames, 0);
    // The drift is made up for, roughly one frame each `1e6 / ppm` frames.
    let drift = (f64::from(MINUTES) * 60.0 * f64::from(SAMPLE_RATE) * ppm.abs() / 1e6) as u64;
    let corrected = stats.dropped_frames.max(stats.inserted_frames);
    assert!(
        corrected + (frames_of(DRIFT_TOLERANCE) + 2 * CHUNK) as u64 >= drift,
        "{corrected} frames corrected for a drift of {drift}"
    );
}

#[test]
fn a_faster_host_does_not_drift_over_thirty_minutes() {
    simulate_drift(300.0);
}

#[test]
fn a_slower_host_does_not_drift_over_thirty_minutes() {
    simulate_drift(-300.0);
}

#[test]
fn odd_sizes_are_safe() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    buffer.push(0, 0, &[], start);
    buffer.push(0, 0, &[1], start);
    assert_eq!(buffer.buffered(), Duration::ZERO);
    fill_to_target(&mut buffer, 0, start);
    let mut out = [i16::MAX; 3];
    buffer.pull(&mut out);
    assert_eq!(out, [0, 0, 0]);
    buffer.pull(&mut []);
}

#[test]
fn datagrams_too_late_to_play_still_raise_the_delay() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(start);
    for n in 0..300u64 {
        let now = start + TICK * n as u32;
        // Every fifth chunk comes 60 ms late: after its turn at 40 ms.
        let late = n % 5 == 4;
        if !late {
            buffer.push(0, n * CHUNK as u64, &chunk(1), now);
        }
        if n >= 6 && (n - 6) % 5 == 4 {
            let chunk_late = n - 6;
            buffer.push(0, chunk_late * CHUNK as u64, &chunk(1), now);
        }
        pull(&mut buffer, CHUNK);
    }
    assert!(buffer.stats().late_frames > 0);
    assert!(buffer.target() > MIN_DELAY, "{:?}", buffer.target());
}
