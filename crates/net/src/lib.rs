//! QUIC transport for wshare: one QUIC connection per viewing session.
//!
//! - **Control channel**: one bidirectional stream carrying length-prefixed
//!   [`protocol::control::ControlMessage`] frames. The *viewer* opens it and
//!   must send first (`Hello`): a QUIC stream only becomes visible to the
//!   peer once data has been written on it.
//! - **Video channel**: unreliable QUIC DATAGRAMs, already packetized by the
//!   caller to at most [`VideoChannel::max_datagram_size`] bytes.
//!
//! Security model: both sides hold a self-signed certificate ([`Identity`])
//! and are identified by its SHA-256 [`Fingerprint`], never by a CA.
//! - The host presents its certificate; the viewer either pins the
//!   fingerprint (a different one fails with [`IdentityMismatch`]) or trusts
//!   on first use and reads it back from [`Session::peer_fingerprint`].
//! - The viewer may present its own certificate as a TLS client
//!   certificate. It is requested but optional, and any self-signed
//!   certificate passes the handshake; the host reads the proven
//!   fingerprint from [`Session::peer_fingerprint`] and decides at the
//!   application layer whether that viewer is already authorized or must
//!   give the PIN in `Hello`.
//!
//! In both directions the handshake signature is verified, so a fingerprint
//! read from a session belongs to a peer that holds the matching private key.
//!
//! Runtime: everything here must run inside a **multi-thread tokio runtime**.
//! quinn spawns its endpoint/connection drivers on the ambient runtime, and
//! [`VideoChannel`]'s synchronous `send` is meant to be called from encoder
//! threads while tokio worker threads drive the actual I/O.
//!
//! Module layout (one reason to change each):
//! - [`identity`]: key/certificate generation, persistence, fingerprints.
//! - `tls`: rustls configuration and the certificate verifiers (host
//!   pinning/TOFU on the viewer, optional client certificates on the host).
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
pub use endpoint::{ConnectError, HostEndpoint, IdentityMismatch, connect};
pub use identity::{Fingerprint, Identity};
pub use session::{Session, SessionHandle};
pub use video::VideoChannel;

/// ALPN protocol identifier. It stays fixed: it only says "this is wshare",
/// so that a QUIC connection reaches the application at all. Compatibility
/// is decided by `Hello.version` (`protocol::control::PROTOCOL_VERSION`),
/// the first message of every session: a host that does not support the
/// version answers `Rejected`, which the viewer can show. For that to work
/// across versions, `Hello` and `Rejected` must remain decodable by every
/// version (see `protocol::control`). Changing the ALPN would instead make
/// the handshake fail with a generic TLS error and no readable reason.
pub const ALPN: &[u8] = b"wshare/1";

/// Server name the viewer puts in the TLS ClientHello. Certificates are
/// verified by fingerprint, not by name, so this is a constant that the
/// host certificate also carries as its DNS SAN.
pub(crate) const SERVER_NAME: &str = "wshare";
