//! QUIC transport parameters tuned for realtime video on a LAN.

use std::sync::Arc;
use std::time::Duration;

use pipeline::backlog::MAX_SEND_BACKLOG;
use quinn::{IdleTimeout, MtuDiscoveryConfig, TransportConfig, VarInt};

/// Keep-alives keep NAT/firewall state warm and let a dead peer be noticed
/// quickly: with the idle timeout below a vanished peer is dropped in ~3 s.
///
/// The idle timeout is how long a network drop takes to be noticed: the
/// viewer starts reconnecting then, and the host frees the place of a viewer
/// that left without saying goodbye. 3 s is three keep-alive intervals, so a
/// single lost packet never ends a session, and a real drop freezes the
/// picture two seconds less than the previous 5 s. The price: a stall of
/// over 3 s with no packets at all (a bad Wi-Fi roam) now ends the
/// connection, which the viewer then reconnects by itself.
const KEEP_ALIVE: Duration = Duration::from_secs(1);
const IDLE_TIMEOUT_MS: u32 = 3_000;
/// Keep-alive intervals the idle timeout must span: fewer and ordinary
/// packet loss would end a healthy session.
const MIN_KEEP_ALIVES_PER_TIMEOUT: u128 = 3;

/// Largest keyframe the datagram buffers must hold whole. The largest
/// picture supported is 4096x2304, the largest frame H.264 levels 5.1 and
/// 5.2 allow: 9.4 million pixels. 2 MiB is 1.8 bits per pixel, more than a
/// hardware encoder spends on an intra frame in low-latency CBR: its rate
/// control buffer is four average frames (`codec::encoder::tuning`), i.e.
/// ~170 KB at 20 Mbit/s and 60 fps, and reaches 2 MiB only at about
/// 250 Mbit/s at 60 fps.
pub(crate) const MAX_KEYFRAME_BYTES: usize = 2 * 1024 * 1024;

/// Send queue for video datagrams: the host stops encoding while more than
/// [`MAX_SEND_BACKLOG`] is queued, so the queue holds at most that plus the
/// last frame admitted, which may be a keyframe. quinn drops the *oldest*
/// queued datagrams only when this is exceeded, which then cannot happen
/// while keyframes stay within [`MAX_KEYFRAME_BYTES`].
/// 3 MiB in all; at 20 Mbit/s the queue stays at a few frames (see
/// `pipeline::backlog`), this is only the hard ceiling.
pub(crate) const DATAGRAM_SEND_BUFFER: usize = MAX_SEND_BACKLOG + MAX_KEYFRAME_BYTES;

/// Received datagrams wait here until the viewer reads them: the viewer
/// drains it continuously, so it only needs to absorb the same burst the
/// sender can queue. Beyond it quinn drops the oldest, again favouring
/// fresh video.
pub(crate) const DATAGRAM_RECEIVE_BUFFER: usize = DATAGRAM_SEND_BUFFER;

// Checked at compile time: the receive buffer holds a whole keyframe, and
// is at least 64 KiB, since quinn also derives from it the largest datagram
// the peer may send.
const _: () = assert!(DATAGRAM_RECEIVE_BUFFER >= MAX_KEYFRAME_BYTES);
const _: () = assert!(DATAGRAM_RECEIVE_BUFFER >= u16::MAX as usize);
const _: () = assert!(
    IDLE_TIMEOUT_MS as u128 >= MIN_KEEP_ALIVES_PER_TIMEOUT * KEEP_ALIVE.as_millis(),
    "the idle timeout must span several keep-alives"
);

/// Shared by host and viewer so both advertise the same limits.
pub(crate) fn transport_config() -> Arc<TransportConfig> {
    let mut config = TransportConfig::default();
    config
        .keep_alive_interval(Some(KEEP_ALIVE))
        .max_idle_timeout(Some(IdleTimeout::from(VarInt::from_u32(IDLE_TIMEOUT_MS))))
        .datagram_send_buffer_size(DATAGRAM_SEND_BUFFER)
        .datagram_receive_buffer_size(Some(DATAGRAM_RECEIVE_BUFFER))
        // Starts at the 1200-byte QUIC minimum and probes up (to ~1452 on
        // Ethernet), so `max_datagram_size` grows shortly after connecting.
        .mtu_discovery_config(Some(MtuDiscoveryConfig::default()))
        // Video is the only bulk traffic; nothing else needs many streams.
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .max_concurrent_bidi_streams(VarInt::from_u32(1));
    Arc::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Largest picture the buffers are sized for, see [`MAX_KEYFRAME_BYTES`].
    const MAX_PIXELS: usize = 4096 * 2304;

    #[test]
    fn send_buffer_is_the_backlog_limit_plus_a_whole_keyframe() {
        assert_eq!(DATAGRAM_SEND_BUFFER, 3 * 1024 * 1024);
        assert_eq!(MAX_SEND_BACKLOG + MAX_KEYFRAME_BYTES, DATAGRAM_SEND_BUFFER);
    }

    #[test]
    fn keyframe_budget_allows_well_over_a_bit_and_a_half_per_pixel() {
        let bits_per_pixel = (MAX_KEYFRAME_BYTES * 8) as f64 / MAX_PIXELS as f64;
        assert!(bits_per_pixel > 1.5, "{bits_per_pixel}");
    }

    #[test]
    fn buffers_are_not_seconds_of_video() {
        // Even if the host ignored the backlog, the ceiling at the default
        // 20 Mbit/s is about a second of video.
        let seconds = (DATAGRAM_SEND_BUFFER * 8) as f64 / 20e6;
        assert!(seconds < 1.5, "{seconds} s");
        let seconds = (DATAGRAM_RECEIVE_BUFFER * 8) as f64 / 20e6;
        assert!(seconds < 1.5, "{seconds} s");
    }
}
