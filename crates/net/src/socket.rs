//! UDP socket creation with buffers sized for video bursts.
//!
//! Windows gives UDP sockets a 64 KB receive buffer by default. A single
//! 1080p keyframe is a few hundred KB sent back-to-back, so with the default
//! the OS silently drops the tail of every keyframe before quinn ever sees
//! it — the viewer then loses the frame and has to ask for another keyframe,
//! which is lost the same way. Large buffers absorb the burst.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use quinn::{Endpoint, EndpointConfig, ServerConfig};
use socket2::{Domain, Protocol, Socket, Type};

/// Requested send/receive buffer size. The OS may clamp it; that is fine.
pub(crate) const SOCKET_BUFFER_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn bind_udp(addr: SocketAddr) -> Result<std::net::UdpSocket> {
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))
        .context("creating UDP socket")?;
    // Best effort: a smaller buffer still works, just less burst-tolerant.
    let _ = socket.set_recv_buffer_size(SOCKET_BUFFER_BYTES);
    let _ = socket.set_send_buffer_size(SOCKET_BUFFER_BYTES);
    socket
        .bind(&addr.into())
        .with_context(|| format!("binding UDP {addr}"))?;
    Ok(socket.into())
}

/// A quinn endpoint on a [`bind_udp`] socket; `server` makes it accept
/// connections.
pub(crate) fn endpoint(addr: SocketAddr, server: Option<ServerConfig>) -> Result<Endpoint> {
    let socket = bind_udp(addr)?;
    let runtime = quinn::default_runtime().context("no async runtime (call inside tokio)")?;
    Endpoint::new(EndpointConfig::default(), server, socket, runtime)
        .with_context(|| format!("creating QUIC endpoint on {addr}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receive_buffer_is_larger_than_windows_default() {
        let socket = bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
        let size = Socket::from(socket).recv_buffer_size().unwrap();
        assert!(size >= 1024 * 1024, "receive buffer only {size} bytes");
    }
}
