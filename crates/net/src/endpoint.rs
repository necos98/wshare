//! Endpoint setup: the host's listening endpoint and the viewer's
//! [`connect`].
//!
//! Address family: the host listens on IPv4 (`0.0.0.0`) by default, see
//! [`HostEndpoint::bind_any`]. A `[::]` socket on Windows is IPv6-only
//! (`IPV6_V6ONLY` defaults to on and quinn does not change it), so
//! "dual-stack" would really mean IPv6-only; LAN discovery and typed-in
//! addresses are IPv4 in practice. Callers that want IPv6 can pass an IPv6
//! address to [`HostEndpoint::bind`].

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{Connection, Endpoint, VarInt};
use rustls::pki_types::CertificateDer;

use crate::identity::{Fingerprint, Identity};
use crate::session::Session;
use crate::tls;
use crate::transport::transport_config;

/// How long the host waits for a freshly connected viewer to open the
/// control stream (i.e. send `Hello`) before dropping it.
const CONTROL_STREAM_TIMEOUT: Duration = Duration::from_secs(5);

/// Listening endpoint of the host (sharing) side.
pub struct HostEndpoint {
    endpoint: Endpoint,
}

impl HostEndpoint {
    /// Binds a QUIC server endpoint on `addr` presenting `identity`.
    /// Must be called inside a tokio runtime (multi-thread).
    pub fn bind(addr: SocketAddr, identity: &Identity) -> Result<Self> {
        let crypto = QuicServerConfig::try_from(tls::server_config(identity)?)
            .context("building QUIC server crypto")?;
        let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        config.transport_config(transport_config());
        let endpoint = crate::socket::endpoint(addr, Some(config))?;
        Ok(Self { endpoint })
    }

    /// Binds on all IPv4 interfaces (`0.0.0.0:port`; 0 = ephemeral).
    pub fn bind_any(port: u16, identity: &Identity) -> Result<Self> {
        Self::bind((Ipv4Addr::UNSPECIFIED, port).into(), identity)
    }

    /// Actual bound address (useful with port 0).
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.endpoint.local_addr().context("querying local address")
    }

    /// Waits for the next viewer whose handshake completes and who opens
    /// the control stream. Connections that fail the handshake or stay
    /// silent for [`CONTROL_STREAM_TIMEOUT`] are dropped and accepting
    /// continues. Errors only once the endpoint is closed. Viewers are
    /// established one at a time: to serve several at once, take each
    /// [`Self::incoming`] connection and establish it on its own task.
    pub async fn accept(&self) -> Result<Session> {
        loop {
            if let Ok(session) = self.incoming().await?.establish().await {
                return Ok(session);
            }
        }
    }

    /// Waits for the next connection attempt, without waiting for its
    /// handshake: the caller decides whether to [`Incoming::establish`] or
    /// [`Incoming::refuse`] it. Errors only once the endpoint is closed.
    pub async fn incoming(&self) -> Result<Incoming> {
        self.endpoint
            .accept()
            .await
            .map(|incoming| Incoming { incoming })
            .ok_or_else(|| anyhow!("host endpoint closed"))
    }

    /// Stops accepting and closes every connection of this endpoint.
    pub fn close(&self) {
        self.endpoint.close(VarInt::from_u32(0), b"host closed");
    }
}

/// A viewer's connection attempt, before its handshake. Dropping it
/// ignores the attempt.
pub struct Incoming {
    incoming: quinn::Incoming,
}

impl Incoming {
    /// Turns the viewer away at once, without a handshake.
    pub fn refuse(self) {
        self.incoming.refuse();
    }

    /// Completes the handshake and waits for the viewer to open the control
    /// stream; one that stays silent for [`CONTROL_STREAM_TIMEOUT`] is
    /// dropped with an error.
    pub async fn establish(self) -> Result<Session> {
        let conn = self.incoming.await.context("QUIC handshake")?;
        // `None`: the viewer presented no certificate (it is optional).
        let viewer = peer_fingerprint(&conn);
        match tokio::time::timeout(CONTROL_STREAM_TIMEOUT, conn.accept_bi()).await {
            Ok(Ok(streams)) => Ok(Session::new(conn, streams, viewer)),
            Ok(Err(e)) => Err(e).context("accepting control stream"),
            Err(_) => {
                conn.close(VarInt::from_u32(1), b"no control stream");
                bail!("viewer did not open the control stream in time")
            }
        }
    }
}

/// The pinned host presented a different certificate: either another
/// machine answers at that address or the host's identity was recreated.
/// Returned (inside the `anyhow::Error`) by [`connect`]; downcast to tell it
/// apart from ordinary connection failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityMismatch {
    pub expected: Fingerprint,
    pub actual: Fingerprint,
}

impl fmt::Display for IdentityMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the host's identity changed: expected fingerprint {}, got {}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for IdentityMismatch {}

/// The QUIC connection to the host could not be established. Returned
/// (inside the `anyhow::Error`) by [`connect`], so callers tell a host that
/// never answered from one that refused without reading the message.
#[derive(Debug)]
pub enum ConnectError {
    /// Nothing answered in time: host off, wrong address or a firewall.
    TimedOut { addr: SocketAddr },
    Failed {
        addr: SocketAddr,
        source: quinn::ConnectionError,
    },
}

impl ConnectError {
    fn new(addr: SocketAddr, source: quinn::ConnectionError) -> Self {
        match source {
            quinn::ConnectionError::TimedOut => Self::TimedOut { addr },
            source => Self::Failed { addr, source },
        }
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut { addr } => write!(f, "connecting to {addr}: timed out"),
            Self::Failed { addr, source } => write!(f, "connecting to {addr}: {source}"),
        }
    }
}

impl std::error::Error for ConnectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TimedOut { .. } => None,
            Self::Failed { source, .. } => Some(source),
        }
    }
}

/// Connects a viewer to the host at `addr`.
///
/// - `expected = Some(fp)`: the handshake fails unless the host certificate
///   has fingerprint `fp` (pinning); a different one is reported as
///   [`IdentityMismatch`].
/// - `expected = None`: trust on first use; read what was seen from
///   [`Session::peer_fingerprint`].
///
/// `identity`, when given, is presented as the TLS client certificate, so
/// the host can recognise this viewer by its fingerprint.
///
/// The control stream is opened locally but is invisible to the host until
/// the first message is sent, so send `Hello` immediately. Must be called
/// inside a tokio runtime (multi-thread).
pub async fn connect(
    addr: SocketAddr,
    expected: Option<Fingerprint>,
    identity: Option<&Identity>,
) -> Result<Session> {
    let verifier = Arc::new(tls::FingerprintVerifier::new(expected));
    let crypto = QuicClientConfig::try_from(tls::client_config(verifier.clone(), identity)?)
        .context("building QUIC client crypto")?;
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(transport_config());

    let local: SocketAddr = if addr.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    };
    // One endpoint per session: dropping our handle is fine, quinn keeps the
    // endpoint driver alive until its connections are gone.
    let endpoint = crate::socket::endpoint(local, None)?;
    let connecting = endpoint
        .connect_with(config, addr, crate::SERVER_NAME)
        .context("starting QUIC connection")?;
    let conn = match connecting.await {
        Ok(conn) => conn,
        Err(e) => {
            if let (Some(expected), Some(actual)) = (expected, verifier.mismatch()) {
                return Err(IdentityMismatch { expected, actual }.into());
            }
            return Err(ConnectError::new(addr, e).into());
        }
    };

    let fingerprint =
        peer_fingerprint(&conn).ok_or_else(|| anyhow!("host presented no certificate"))?;
    let streams = conn.open_bi().await.context("opening control stream")?;
    Ok(Session::new(conn, streams, Some(fingerprint)))
}

/// Fingerprint of the leaf certificate the peer proved during the
/// handshake; `None` if it presented none.
fn peer_fingerprint(conn: &Connection) -> Option<Fingerprint> {
    let certs = conn
        .peer_identity()?
        .downcast::<Vec<CertificateDer<'static>>>()
        .ok()?;
    certs.first().map(|leaf| Fingerprint::of_cert(leaf))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: &str = "10.0.0.9:7420";

    #[test]
    fn a_quic_timeout_is_told_apart_from_other_failures() {
        let addr: SocketAddr = ADDR.parse().unwrap();
        let timed_out = ConnectError::new(addr, quinn::ConnectionError::TimedOut);
        assert!(matches!(timed_out, ConnectError::TimedOut { .. }));
        assert_eq!(
            timed_out.to_string(),
            "connecting to 10.0.0.9:7420: timed out"
        );
        let reset = ConnectError::new(addr, quinn::ConnectionError::Reset);
        assert!(matches!(reset, ConnectError::Failed { .. }));
        assert!(
            reset
                .to_string()
                .starts_with("connecting to 10.0.0.9:7420: ")
        );
    }
}
