//! Control channel: [`ControlMessage`] frames over the session's single
//! bidirectional QUIC stream. Framing is owned by `protocol::control`; this
//! module only moves bytes.

use anyhow::{Context, Result};
use protocol::control::{ControlMessage, FrameDecoder, encode_frame};
use quinn::{ConnectionError, ReadError, RecvStream, SendStream};

/// Read chunk size; control messages are tiny, this just bounds one read.
const READ_CHUNK: usize = 4096;

/// Sending half of the control channel.
pub struct ControlSender {
    stream: SendStream,
}

impl ControlSender {
    pub(crate) fn new(stream: SendStream) -> Self {
        Self { stream }
    }

    /// Sends one message (reliable, in order). On the viewer side the first
    /// call is what makes the stream visible to the host, so send `Hello`
    /// right after connecting.
    pub async fn send(&mut self, msg: &ControlMessage) -> Result<()> {
        self.stream
            .write_all(&encode_frame(msg))
            .await
            .context("writing control message")
    }

    /// Gracefully ends the sending direction; the peer's
    /// [`ControlReceiver::recv`] then returns `Ok(None)`.
    pub fn finish(&mut self) -> Result<()> {
        self.stream.finish().context("finishing control stream")
    }
}

/// Receiving half of the control channel.
pub struct ControlReceiver {
    stream: RecvStream,
    decoder: FrameDecoder,
    buf: Vec<u8>,
}

impl ControlReceiver {
    pub(crate) fn new(stream: RecvStream) -> Self {
        Self {
            stream,
            decoder: FrameDecoder::new(),
            buf: vec![0; READ_CHUNK],
        }
    }

    /// Next message, or `Ok(None)` when the peer finished the stream or
    /// closed the connection normally (application close, or we closed it
    /// locally). Malformed frames, resets and transport errors are errors.
    /// Cancel-safe: no decoded message is lost if the future is dropped.
    pub async fn recv(&mut self) -> Result<Option<ControlMessage>> {
        loop {
            if let Some(msg) = self
                .decoder
                .next_message()
                .context("decoding control message")?
            {
                return Ok(Some(msg));
            }
            match self.stream.read(&mut self.buf).await {
                Ok(Some(n)) => self.decoder.push(&self.buf[..n]),
                Ok(None) => return Ok(None),
                Err(ReadError::ConnectionLost(
                    ConnectionError::ApplicationClosed(_) | ConnectionError::LocallyClosed,
                )) => return Ok(None),
                Err(e) => return Err(e).context("reading control stream"),
            }
        }
    }
}
