//! Video channel: unreliable QUIC datagrams.
//!
//! Datagrams are congestion-controlled but never retransmitted by QUIC:
//! the viewer asks for the ones it missed on the control stream, and the
//! host sends copies while they are still useful (`pipeline::retransmit`),
//! a choice QUIC cannot make for us. The host
//! reads the queue with [`DatagramSink::backlog_bytes`] and stops encoding
//! while it is over a few frames (`pipeline::backlog`), so video does not
//! pile up when the network is slower than the stream; should the queue
//! still overflow, quinn drops the oldest queued datagrams, the right
//! policy for live video (fresh data beats stale data).
//!
//! Control messages share the connection: quinn fills each packet with
//! queued datagrams before stream data, so a host -> viewer message (a
//! cursor shape, `InputAllowed`, `Bye`) waits behind whatever video is
//! queued. With the backlog bounded, that is at most the backlog limit plus
//! the last frame admitted, i.e. a few frame times at the stream rate, not
//! seconds.

use anyhow::{Context, Result};
use bytes::Bytes;
use pipeline::traits::{DatagramSink, DatagramTooLarge};
use quinn::{Connection, SendDatagramError};

use crate::file_activity::FileActivity;
use crate::stats::TransportStats;
use crate::transport::DATAGRAM_SEND_BUFFER;

/// Used when the connection cannot report a limit (e.g. datagrams not
/// negotiated); the QUIC minimum MTU minus headers fits in it comfortably.
const FALLBACK_MAX_DATAGRAM: usize = 1200;

/// Sends and receives video datagrams. Cheap to clone (shares the
/// connection).
#[derive(Clone)]
pub struct VideoChannel {
    conn: Connection,
    /// The file streams sharing the connection, reported with its statistics.
    files: FileActivity,
}

impl VideoChannel {
    pub(crate) fn new(conn: Connection, files: FileActivity) -> Self {
        Self { conn, files }
    }

    /// Largest datagram that currently fits in one packet. Grows after MTU
    /// discovery, so packetizers should re-query it periodically.
    pub fn max_datagram_size(&self) -> usize {
        self.conn
            .max_datagram_size()
            .unwrap_or(FALLBACK_MAX_DATAGRAM)
    }

    /// Waits for the next datagram. Errors once the connection is gone.
    pub async fn recv(&self) -> Result<Bytes> {
        self.conn
            .read_datagram()
            .await
            .context("receiving video datagram")
    }

    /// Statistics of the connection carrying the video, for fitting the
    /// stream's bitrate to the path. Never blocks.
    pub fn transport_stats(&self) -> TransportStats {
        TransportStats::of(&self.conn, &self.files)
    }
}

impl DatagramSink for VideoChannel {
    fn max_datagram_size(&self) -> usize {
        VideoChannel::max_datagram_size(self)
    }

    /// Never blocks: queues the datagram for the connection driver. Datagrams
    /// dropped later under congestion are not reported. Errors with a
    /// [`DatagramTooLarge`] when the datagram exceeds the current limit
    /// (quinn lowers it when it suspects the path's MTU), otherwise when
    /// the peer does not accept datagrams or the connection is lost.
    fn send(&self, datagram: Bytes) -> Result<()> {
        let len = datagram.len();
        match self.conn.send_datagram(datagram) {
            Ok(()) => Ok(()),
            Err(SendDatagramError::TooLarge) => Err(DatagramTooLarge {
                len,
                limit: self.max_datagram_size(),
            }
            .into()),
            Err(e) => Err(e).context("sending video datagram"),
        }
    }

    /// The part of the send buffer in use. quinn also counts a few dozen
    /// bytes of bookkeeping per datagram (and one entry's worth even when
    /// empty), negligible against the backlog limits in use.
    fn backlog_bytes(&self) -> usize {
        DATAGRAM_SEND_BUFFER.saturating_sub(self.conn.datagram_send_buffer_space())
    }
}
