//! QUIC transport parameters tuned for realtime video on a LAN.

use std::sync::Arc;
use std::time::Duration;

use quinn::{IdleTimeout, MtuDiscoveryConfig, TransportConfig, VarInt};

/// Keep-alives keep NAT/firewall state warm and let a dead peer be noticed
/// quickly: with the idle timeout below a vanished viewer is dropped in ~5 s.
const KEEP_ALIVE: Duration = Duration::from_secs(1);
const IDLE_TIMEOUT_MS: u32 = 5_000;

/// Datagram buffers hold a burst of several keyframes (a 4K keyframe can be
/// 1-2 MB). On the send side quinn drops the *oldest* datagrams when this
/// fills, so it bounds latency as well as memory.
const DATAGRAM_SEND_BUFFER: usize = 16 * 1024 * 1024;
const DATAGRAM_RECEIVE_BUFFER: usize = 16 * 1024 * 1024;

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
