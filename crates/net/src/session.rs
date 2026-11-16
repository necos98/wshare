//! A connected viewing session: one QUIC connection with its control stream
//! already established.

use std::net::SocketAddr;
use std::time::Duration;

use quinn::{Connection, RecvStream, SendStream, VarInt};

use crate::control::{ControlReceiver, ControlSender};
use crate::files::FileChannel;
use crate::identity::Fingerprint;
use crate::video::VideoChannel;

/// QUIC application error code for an orderly close.
const CLOSE_NORMAL: VarInt = VarInt::from_u32(0);

/// One viewing session, as returned by [`crate::HostEndpoint::accept`] or
/// [`crate::connect`]. Use [`Session::split`] to get the channels; keep a
/// [`SessionHandle`] (from [`Session::handle`]) to close the session or wait
/// for it to end after splitting.
pub struct Session {
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
    peer_fingerprint: Option<Fingerprint>,
}

impl Session {
    pub(crate) fn new(
        conn: Connection,
        (send, recv): (SendStream, RecvStream),
        peer_fingerprint: Option<Fingerprint>,
    ) -> Self {
        Self {
            conn,
            send,
            recv,
            peer_fingerprint,
        }
    }

    pub fn remote_addr(&self) -> SocketAddr {
        self.conn.remote_address()
    }

    /// Fingerprint of the certificate the peer proved during the handshake.
    ///
    /// - Viewer side: the host's (always present). With trust-on-first-use,
    ///   store this and pin it next time.
    /// - Host side: the viewer's client certificate, or `None` if it
    ///   presented none (client certificates are optional).
    pub fn peer_fingerprint(&self) -> Option<Fingerprint> {
        self.peer_fingerprint
    }

    /// A cloneable handle for closing / observing the connection.
    pub fn handle(&self) -> SessionHandle {
        SessionHandle {
            conn: self.conn.clone(),
        }
    }

    /// Closes the connection immediately (pending data may be lost; send
    /// `Bye` first for a graceful end).
    pub fn close(&self) {
        self.handle().close("bye");
    }

    /// Splits into the control sender/receiver, the video channel and the
    /// file channel.
    pub fn split(self) -> (ControlSender, ControlReceiver, VideoChannel, FileChannel) {
        (
            ControlSender::new(self.send),
            ControlReceiver::new(self.recv),
            VideoChannel::new(self.conn.clone()),
            FileChannel::new(self.conn),
        )
    }
}

/// Cloneable handle on a session's connection, usable after
/// [`Session::split`].
#[derive(Clone)]
pub struct SessionHandle {
    conn: Connection,
}

impl SessionHandle {
    pub fn remote_addr(&self) -> SocketAddr {
        self.conn.remote_address()
    }

    /// Current round-trip time estimate.
    pub fn rtt(&self) -> Duration {
        self.conn.rtt()
    }

    /// Closes the connection with an orderly application close; `reason` is
    /// sent to the peer for diagnostics.
    pub fn close(&self, reason: &str) {
        self.conn.close(CLOSE_NORMAL, reason.as_bytes());
    }

    /// Resolves when the connection ends, with a human-readable cause.
    pub async fn closed(&self) -> String {
        self.conn.closed().await.to_string()
    }
}
