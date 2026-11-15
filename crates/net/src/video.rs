//! Video channel: unreliable QUIC datagrams.
//!
//! Datagrams are congestion-controlled but never retransmitted. The host
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

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use pipeline::traits::DatagramSink;
use quinn::{Connection, SendDatagramError};

use crate::transport::DATAGRAM_SEND_BUFFER;

/// Used when the connection cannot report a limit (e.g. datagrams not
/// negotiated); the QUIC minimum MTU minus headers fits in it comfortably.
const FALLBACK_MAX_DATAGRAM: usize = 1200;

/// Sends and receives video datagrams. Cheap to clone (shares the
/// connection).
#[derive(Clone)]
pub struct VideoChannel {
    conn: Connection,
}

impl VideoChannel {
    pub(crate) fn new(conn: Connection) -> Self {
        Self { conn }
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
}

impl DatagramSink for VideoChannel {
    fn max_datagram_size(&self) -> usize {
        VideoChannel::max_datagram_size(self)
    }

    /// Never blocks: queues the datagram for the connection driver. Datagrams
    /// dropped later under congestion are not reported. Errors when the
    /// datagram exceeds the current limit, the peer does not accept
    /// datagrams, or the connection is lost.
    fn send(&self, datagram: Vec<u8>) -> Result<()> {
        let len = datagram.len();
        match self.conn.send_datagram(Bytes::from(datagram)) {
            Ok(()) => Ok(()),
            Err(SendDatagramError::TooLarge) => bail!(
                "video datagram of {len} bytes exceeds the current limit of {}",
                self.max_datagram_size()
            ),
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
