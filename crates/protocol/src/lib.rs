//! Wire formats shared by host and viewer.
//!
//! - [`audio`]: unreliable audio datagrams (the host's sound, as PCM).
//! - [`control`]: reliable control-channel messages (QUIC stream).
//! - [`feedback`]: the viewer's reception reports, for rate adaptation.
//! - [`input`]: remote-control payloads (input events, cursor shape).
//! - [`recovery`]: the viewer's requests to resend lost video datagrams.
//! - [`session`]: what the viewer chooses mid-session (monitor, quality).
//! - [`transfer`]: files pushed or pasted between the two PCs.
//! - [`video`]: unreliable video datagrams, their reassembly and loss policy.

pub mod audio;
pub mod control;
pub mod feedback;
pub mod input;
pub mod recovery;
pub mod session;
pub mod transfer;
pub mod video;

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("datagram too short ({0} bytes)")]
    Truncated(usize),
    #[error("unsupported protocol version {0}")]
    BadVersion(u8),
    #[error("malformed packet: {0}")]
    Malformed(&'static str),
    #[error("control message too large ({0} bytes)")]
    TooLarge(usize),
    /// A `Hello` this build cannot decode because its sender speaks another
    /// protocol version (the version is read from the part of `Hello` that
    /// never changes).
    #[error("Hello of protocol version {0} cannot be decoded")]
    IncompatibleHello(u16),
    #[error("cannot decode control message: {0}")]
    Decode(String),
}
