//! What the transport knows about the path, in plain types: the host fits
//! its bitrate to it, the viewer shows it.

use std::time::Duration;

use quinn::Connection;

/// A snapshot of one connection's transport statistics. The counters are
/// cumulative since the connection opened: diff two snapshots to get the
/// rates of an interval.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransportStats {
    /// Smoothed round-trip time estimate.
    pub rtt: Duration,
    /// Lowest round-trip time seen: the path's latency without queueing.
    pub min_rtt: Duration,
    /// Congestion window, in bytes.
    pub congestion_window: u64,
    /// Packets declared lost (MTU probes excluded).
    pub lost_packets: u64,
    /// Packets sent, MTU probes included.
    pub sent_packets: u64,
}

impl TransportStats {
    pub(crate) fn of(conn: &Connection) -> Self {
        let path = conn.stats().path;
        Self {
            rtt: path.rtt,
            min_rtt: path.min_rtt,
            congestion_window: path.cwnd,
            lost_packets: path.lost_packets,
            sent_packets: path.sent_packets,
        }
    }
}
