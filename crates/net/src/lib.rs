//! QUIC transport for wshare: one QUIC connection per viewing session.
//!
//! - **Control channel**: one bidirectional stream carrying length-prefixed
//!   [`protocol::control::ControlMessage`] frames. The *viewer* opens it and
//!   must send first (`Hello`): a QUIC stream only becomes visible to the
//!   peer once data has been written on it.
//! - **Video channel**: unreliable QUIC DATAGRAMs, already packetized by the
//!   caller to at most [`VideoChannel::max_datagram_size`] bytes.
//!
//! Security model: the host presents a self-signed certificate
//! ([`Identity`]); the viewer either pins its SHA-256 [`Fingerprint`] or
//! trusts on first use and reads it back from
//! [`Session::peer_fingerprint`]. Viewers are authorized at the application
//! layer (PIN in `Hello`), so no client certificates are used.
//!
//! Runtime: everything here must run inside a **multi-thread tokio runtime**.
//! quinn spawns its endpoint/connection drivers on the ambient runtime, and
//! [`VideoChannel`]'s synchronous `send` is meant to be called from encoder
//! threads while tokio worker threads drive the actual I/O.
//!
//! Module layout (one reason to change each):
//! - [`identity`]: key/certificate generation, persistence, fingerprints.
//! - `tls`: rustls configuration and the pinning/TOFU certificate verifier.
//! - `transport`: QUIC transport parameters tuned for LAN video.
//! - [`endpoint`]: host endpoint and viewer [`connect`].
//! - [`session`]: a connected session and its handle.
//! - [`control`] / [`video`]: channel wrappers.

pub mod control;
pub mod endpoint;
pub mod identity;
pub mod session;
mod socket;
mod tls;
mod transport;
pub mod video;

pub use control::{ControlReceiver, ControlSender};
pub use endpoint::{HostEndpoint, connect};
pub use identity::{Fingerprint, Identity};
pub use session::{Session, SessionHandle};
pub use video::VideoChannel;

/// ALPN protocol identifier; bump together with breaking wire changes.
pub const ALPN: &[u8] = b"wshare/1";

/// Server name the viewer puts in the TLS ClientHello. Certificates are
/// verified by fingerprint, not by name, so this is a constant that the
/// host certificate also carries as its DNS SAN.
pub(crate) const SERVER_NAME: &str = "wshare";
