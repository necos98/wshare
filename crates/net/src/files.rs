//! File channel: one QUIC unidirectional stream per file, opened by the side
//! that has the file. A stream starts with its
//! [`protocol::transfer::StreamHeader`] and carries the file's bytes; flow
//! control is the backpressure (a writer waits while the reader is behind).
//!
//! File streams have a lower priority than the control stream, and video
//! travels in datagrams, which quinn packs before any stream data: within
//! the congestion window the video goes first. The window and the path are
//! shared all the same, and nothing limits a file's rate: a large file fills
//! the window and the queue at the path's bottleneck (a Wi-Fi access point),
//! so the whole connection sees a longer round trip and losses, and the
//! video waits in that queue too. The open file streams are counted
//! ([`crate::TransportStats::file_streams_open`]) so that the host's bitrate
//! adaptation does not take those signals for the video's own congestion.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use protocol::transfer::{
    HEADER_LENGTH_PREFIX, StreamHeader, decode_header, encode_header, header_length,
};
use quinn::{Connection, ReadExactError, RecvStream, SendStream, VarInt};

use crate::file_activity::{FileActivity, OpenFileStream};

/// Below the control stream's priority (0): control messages go first.
const FILE_PRIORITY: i32 = -1;

/// Application error code of a stream given up on purpose (cancelled).
const CANCELLED: VarInt = VarInt::from_u32(1);

/// How long the header of a new stream may take to arrive: a peer that
/// opens streams and sends nothing must not hold them.
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);

/// Most bytes one read returns.
pub const MAX_READ_CHUNK: usize = 256 * 1024;

/// Opens and accepts the file streams of one connection. Cloneable: every
/// transfer task holds one.
#[derive(Clone)]
pub struct FileChannel {
    conn: Connection,
    activity: FileActivity,
}

impl FileChannel {
    pub(crate) fn new(conn: Connection, activity: FileActivity) -> Self {
        Self { conn, activity }
    }

    /// Opens a stream for the file `header` describes and sends the header.
    /// Waits while as many streams as the peer allows are open.
    pub async fn open(&self, header: &StreamHeader) -> Result<FileSender> {
        let mut stream = self
            .conn
            .open_uni()
            .await
            .context("opening a file stream")?;
        let open = self.activity.track();
        stream
            .set_priority(FILE_PRIORITY)
            .context("lowering the file stream's priority")?;
        stream
            .write_all(&encode_header(header))
            .await
            .context("sending a file header")?;
        Ok(FileSender {
            stream,
            _open: open,
        })
    }

    /// The next file stream the peer opened; `None` once the connection is
    /// over (any connection error means no stream will come again). Its
    /// header is read by [`IncomingFile::open`], so a slow stream does not
    /// hold up the next ones.
    pub async fn accept(&self) -> Option<IncomingFile> {
        let stream = self.conn.accept_uni().await.ok()?;
        Some(IncomingFile {
            stream,
            open: self.activity.track(),
        })
    }
}

/// A file stream the peer opened, its header not read yet.
pub struct IncomingFile {
    stream: RecvStream,
    open: OpenFileStream,
}

impl IncomingFile {
    /// Reads the header (within a time limit); a stream without a valid one
    /// is stopped.
    pub async fn open(mut self) -> Result<(StreamHeader, FileReceiver)> {
        let header = tokio::time::timeout(HEADER_TIMEOUT, read_header(&mut self.stream))
            .await
            .map_err(|_| anyhow!("no file header within {} s", HEADER_TIMEOUT.as_secs()))
            .and_then(|header| header);
        match header {
            Ok(header) => Ok((
                header,
                FileReceiver {
                    stream: self.stream,
                    _open: self.open,
                },
            )),
            Err(e) => {
                let _ = self.stream.stop(CANCELLED);
                Err(e)
            }
        }
    }
}

async fn read_header(stream: &mut RecvStream) -> Result<StreamHeader> {
    let mut prefix = [0; HEADER_LENGTH_PREFIX];
    stream.read_exact(&mut prefix).await.map_err(header_error)?;
    let mut payload = vec![0; header_length(prefix)?];
    stream
        .read_exact(&mut payload)
        .await
        .map_err(header_error)?;
    Ok(decode_header(&payload)?)
}

fn header_error(error: ReadExactError) -> anyhow::Error {
    anyhow!("reading a file header: {error}")
}

/// Sending end of one file stream.
pub struct FileSender {
    stream: SendStream,
    /// Counted until the file is over.
    _open: OpenFileStream,
}

impl FileSender {
    /// Sends `bytes`, waiting while the receiver is behind.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.stream
            .write_all(bytes)
            .await
            .context("sending file data")
    }

    /// Ends the file and waits until the peer received all of it (or gave
    /// up on it, an error).
    pub async fn finish(mut self) -> Result<()> {
        self.stream.finish().context("ending a file stream")?;
        match self.stream.stopped().await {
            Ok(None) => Ok(()),
            Ok(Some(_)) => Err(anyhow!("the other computer stopped the file")),
            Err(e) => Err(e).context("waiting for the file to arrive"),
        }
    }

    /// Gives the file up: the peer's reads fail at once.
    pub fn cancel(mut self) {
        // Fails only if the stream is already over: nothing left to stop.
        let _ = self.stream.reset(CANCELLED);
    }
}

/// Receiving end of one file stream.
pub struct FileReceiver {
    stream: RecvStream,
    /// Counted until the file is over.
    _open: OpenFileStream,
}

impl FileReceiver {
    /// The next bytes (at most [`MAX_READ_CHUNK`]), `Ok(None)` at the end
    /// of the file. A stream the sender gave up on is an error.
    pub async fn read(&mut self) -> Result<Option<Bytes>> {
        let chunk = self
            .stream
            .read_chunk(MAX_READ_CHUNK, true)
            .await
            .context("receiving file data")?;
        Ok(chunk.map(|chunk| chunk.bytes))
    }

    /// Gives the file up: the sender's writes fail at once.
    pub fn cancel(mut self) {
        // Fails only if the stream is already over: nothing left to stop.
        let _ = self.stream.stop(CANCELLED);
    }
}
