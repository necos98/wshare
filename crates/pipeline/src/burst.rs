//! Spreading a large frame's datagrams over a few milliseconds.
//!
//! quinn paces packets, but on a LAN its pacer lets a keyframe through in
//! one burst: it allows a congestion window's worth of bytes per 2 ms
//! scaled by the round-trip time, clamped to 10..=256 packets
//! (`quinn-proto` 0.11, `connection/pacing.rs`). With a sub-millisecond
//! RTT that is always the 256-packet ceiling, ~370 KB, more than a whole
//! keyframe at the default bitrate. Switches and the receiving socket then
//! see ~170 KB at line rate, which is when datagrams get dropped. The host
//! therefore sends a large frame in a few bursts of at most about
//! [`BURST_BYTES`], [`BURST_GAP`] apart, spread over [`MAX_SPREAD`] at
//! most. Ordinary frames fit in one burst and leave at once: only the rare
//! large frames arrive (by at most [`MAX_SPREAD`]) later.

use std::time::Duration;

/// Bytes sent back to back before a pause: ~45 full datagrams, which a
/// switch port or a socket buffer absorbs without dropping.
pub const BURST_BYTES: usize = 64 * 1024;

/// Pause between two bursts of one frame.
pub const BURST_GAP: Duration = Duration::from_millis(1);

/// Longest time a frame's datagrams are spread over: a quarter of a frame
/// interval at 60 fps. A frame needing more bursts gets larger ones.
pub const MAX_SPREAD: Duration = Duration::from_millis(4);

/// Most bursts one frame is split into.
const MAX_BURSTS: usize = (MAX_SPREAD.as_millis() / BURST_GAP.as_millis()) as usize + 1;

/// How many of the datagrams, of sizes `lengths` in sending order, go in
/// each burst (every burst has at least one; the counts add up to
/// `lengths.len()`). A frame within [`BURST_BYTES`] is one burst.
pub fn burst_sizes(lengths: &[usize]) -> Vec<usize> {
    let total: usize = lengths.iter().sum();
    let largest = lengths.iter().copied().max().unwrap_or(0);
    // Every closed burst then holds more than a `MAX_BURSTS`th of the
    // frame, so there are at most `MAX_BURSTS`.
    let budget = BURST_BYTES.max(total.div_ceil(MAX_BURSTS) + largest);
    let mut sizes = Vec::new();
    let (mut count, mut bytes) = (0, 0);
    for &length in lengths {
        if count > 0 && bytes + length > budget {
            sizes.push(count);
            (count, bytes) = (0, 0);
        }
        count += 1;
        bytes += length;
    }
    if count > 0 {
        sizes.push(count);
    }
    sizes
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATAGRAM: usize = 1400;

    fn frame(bytes: usize) -> Vec<usize> {
        let mut lengths = vec![DATAGRAM; bytes / DATAGRAM];
        if !bytes.is_multiple_of(DATAGRAM) {
            lengths.push(bytes % DATAGRAM);
        }
        lengths
    }

    #[test]
    fn an_ordinary_frame_leaves_in_one_burst() {
        // A delta frame at 20 Mbit/s and 60 fps is ~42 KB.
        assert_eq!(burst_sizes(&frame(42_000)), vec![30]);
        assert_eq!(burst_sizes(&frame(BURST_BYTES)).len(), 1);
    }

    #[test]
    fn a_keyframe_is_split_into_bursts_of_at_most_the_budget() {
        // A keyframe of a four-frame rate-control buffer: ~170 KB.
        let lengths = frame(170_000);
        let sizes = burst_sizes(&lengths);
        assert_eq!(sizes.len(), 3);
        assert_eq!(sizes.iter().sum::<usize>(), lengths.len());
        assert!(sizes.iter().all(|&n| n * DATAGRAM <= BURST_BYTES));
    }

    #[test]
    fn a_huge_frame_is_never_spread_over_more_than_the_maximum() {
        let lengths = frame(2 * 1024 * 1024);
        let sizes = burst_sizes(&lengths);
        assert!(sizes.len() <= MAX_BURSTS, "{} bursts", sizes.len());
        assert_eq!(sizes.iter().sum::<usize>(), lengths.len());
        let spread = BURST_GAP * (sizes.len() as u32 - 1);
        assert!(spread <= MAX_SPREAD);
    }

    #[test]
    fn degenerate_frames() {
        assert!(burst_sizes(&[]).is_empty());
        assert_eq!(burst_sizes(&[0]), vec![1]);
        assert_eq!(burst_sizes(&[BURST_BYTES * 2]), vec![1]);
    }
}
