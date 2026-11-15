//! How much encoded video may wait in the send queue.
//!
//! When the network is slower than the stream, queued video only gets
//! older. The host therefore stops *producing* frames while the queue is
//! over a small limit, instead of queueing them or dropping encoded ones
//! (an encoded frame that never arrives breaks the decoder's reference chain
//! until the next keyframe). The latency added by the queue then stays
//! around [`BACKLOG_FRAMES`] frames, whatever the network does.

/// Frames' worth of video allowed to wait before the host skips frames.
/// Enough to absorb the usual bursts (a keyframe, a large screen change)
/// without skipping on a healthy network; few enough that the picture is
/// never more than a few frames behind.
pub const BACKLOG_FRAMES: u32 = 3;

/// Ceiling of the limit at any bitrate. A transport whose send buffer holds
/// this plus one whole keyframe never has to discard queued video: the
/// queue only grows past the limit by the frame admitted just below it.
pub const MAX_SEND_BACKLOG: usize = 1024 * 1024;

/// Backlog above which the host skips frames: [`BACKLOG_FRAMES`] average
/// frames of a `bitrate_bps` stream at `fps`, capped at
/// [`MAX_SEND_BACKLOG`]. A zero `fps` counts as 1.
pub fn send_backlog_limit(bitrate_bps: u32, fps: u32) -> usize {
    let frame_bytes = u64::from(bitrate_bps) / 8 / u64::from(fps.max(1));
    let limit = frame_bytes.saturating_mul(u64::from(BACKLOG_FRAMES));
    usize::try_from(limit).map_or(MAX_SEND_BACKLOG, |limit| limit.min(MAX_SEND_BACKLOG))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_is_a_few_frames_at_the_stream_rate() {
        // 20 Mbit/s at 60 fps: 41 666 bytes per frame.
        assert_eq!(send_backlog_limit(20_000_000, 60), 41_666 * 3);
        assert_eq!(send_backlog_limit(8_000, 1), 3_000);
    }

    #[test]
    fn limit_at_the_default_rate_is_well_under_a_tenth_of_a_second() {
        let limit = send_backlog_limit(20_000_000, 60);
        let seconds = limit as f64 * 8.0 / 20_000_000.0;
        assert!(seconds < 0.1, "{seconds} s of video may queue");
    }

    #[test]
    fn limit_is_capped_for_huge_bitrates() {
        assert_eq!(send_backlog_limit(u32::MAX, 1), MAX_SEND_BACKLOG);
        assert_eq!(send_backlog_limit(1_000_000_000, 30), MAX_SEND_BACKLOG);
    }

    #[test]
    fn zero_fps_counts_as_one() {
        assert_eq!(send_backlog_limit(8_000, 0), send_backlog_limit(8_000, 1));
    }
}
