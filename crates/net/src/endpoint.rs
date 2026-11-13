//! Endpoint setup: the host's listening endpoint and the viewer's
//! [`connect`].
//!
//! Address family: the host listens on IPv4 (`0.0.0.0`) by default, see
//! [`HostEndpoint::bind_any`]. A `[::]` socket on Windows is IPv6-only
//! (`IPV6_V6ONLY` defaults to on and quinn does not change it), so
//! "dual-stack" would really mean IPv6-only; LAN discovery and typed-in
//! addresses are IPv4 in practice. Callers that want IPv6 can pass an IPv6
//! address to [`HostEndpoint::bind`].

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{Connection, Endpoint, Incoming, VarInt};
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
    /// continues, so one bad peer cannot stop the host. Errors only once the
    /// endpoint is closed. Viewers are handled one at a time.
    pub async fn accept(&self) -> Result<Session> {
        loop {
            let incoming = self
                .endpoint
                .accept()
                .await
                .ok_or_else(|| anyhow!("host endpoint closed"))?;
            if let Ok(session) = Self::establish(incoming).await {
                return Ok(session);
            }
        }
    }

    async fn establish(incoming: Incoming) -> Result<Session> {
        let conn = incoming.await.context("QUIC handshake")?;
        match tokio::time::timeout(CONTROL_STREAM_TIMEOUT, conn.accept_bi()).await {
            Ok(Ok(streams)) => Ok(Session::new(conn, streams, None)),
            Ok(Err(e)) => Err(e).context("accepting control stream"),
            Err(_) => {
                conn.close(VarInt::from_u32(1), b"no control stream");
                bail!("viewer did not open the control stream in time")
            }
        }
    }

    /// Stops accepting and closes every connection of this endpoint.
    pub fn close(&self) {
        self.endpoint.close(VarInt::from_u32(0), b"host closed");
    }
}

/// Connects a viewer to the host at `addr`.
///
/// - `expected = Some(fp)`: the handshake fails unless the host certificate
///   has fingerprint `fp` (pinning).
/// - `expected = None`: trust on first use; read what was seen from
///   [`Session::peer_fingerprint`].
///
/// The control stream is opened locally but is invisible to the host until
/// the first message is sent, so send `Hello` immediately. Must be called
/// inside a tokio runtime (multi-thread).
pub async fn connect(addr: SocketAddr, expected: Option<Fingerprint>) -> Result<Session> {
    let crypto = QuicClientConfig::try_from(tls::client_config(expected)?)
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
    let conn = endpoint
        .connect_with(config, addr, crate::SERVER_NAME)
        .context("starting QUIC connection")?
        .await
        .with_context(|| format!("connecting to {addr}"))?;

    let fingerprint = peer_fingerprint(&conn)?;
    let streams = conn.open_bi().await.context("opening control stream")?;
    Ok(Session::new(conn, streams, Some(fingerprint)))
}

fn peer_fingerprint(conn: &Connection) -> Result<Fingerprint> {
    let certs = conn
        .peer_identity()
        .and_then(|any| any.downcast::<Vec<CertificateDer<'static>>>().ok())
        .ok_or_else(|| anyhow!("host presented no certificate"))?;
    let leaf = certs
        .first()
        .ok_or_else(|| anyhow!("host presented an empty certificate chain"))?;
    Ok(Fingerprint::of_cert(leaf))
}
