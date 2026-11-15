//! Wire formats shared by host and viewer.
//!
//! - [`control`]: reliable control-channel messages (QUIC stream).
//! - [`input`]: remote-control payloads (input events, cursor shape).
//! - [`video`]: unreliable video datagrams, their reassembly and loss policy.

pub mod control;
pub mod input;
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
