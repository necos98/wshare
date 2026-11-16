//! Moving one file's bytes between the disk (or Explorer) and its stream,
//! with progress and cancellation. Disk reads run on the blocking pool.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use net::{FileChannel, FileReceiver};
use protocol::transfer::StreamHeader;
use tokio::sync::{mpsc, watch};

use super::part_file::{PartFile, blocking};
use super::registry::Chunk;

/// Bytes read from the disk at a time.
const READ_CHUNK: usize = 256 * 1024;

/// Why moving a file stopped early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamError {
    /// The transfer was cancelled (by either side) or abandoned.
    Cancelled,
    Failed(String),
}

impl StreamError {
    fn failed(what: &str, error: impl std::fmt::Display) -> Self {
        Self::Failed(format!("{what}: {error}"))
    }
}

/// Resolves once `cancel` is raised (or its sender is gone).
pub async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    let _ = cancel.wait_for(|cancelled| *cancelled).await;
}

/// Sends the file at `source`, exactly `header.entry.size` bytes of it, on
/// a new stream; returns once the other PC received all of it.
pub async fn send_file(
    files: &FileChannel,
    header: &StreamHeader,
    source: &Path,
    mut cancel: watch::Receiver<bool>,
    progress: &AtomicU64,
) -> Result<(), StreamError> {
    let mut reader = SourceFile::open(source.to_path_buf(), header.entry.size).await?;
    let mut stream = tokio::select! {
        () = cancelled(&mut cancel) => return Err(StreamError::Cancelled),
        opened = files.open(header) => opened.map_err(|e| StreamError::failed("opening", e))?,
    };
    loop {
        let chunk = match reader.next().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(e) => {
                stream.cancel();
                return Err(e);
            }
        };
        tokio::select! {
            () = cancelled(&mut cancel) => {
                stream.cancel();
                return Err(StreamError::Cancelled);
            }
            written = stream.write(&chunk) => {
                written.map_err(|e| StreamError::failed("sending", e))?;
            }
        }
        progress.fetch_add(chunk.len() as u64, Ordering::Relaxed);
    }
    tokio::select! {
        () = cancelled(&mut cancel) => Err(StreamError::Cancelled),
        finished = stream.finish() => finished.map_err(|e| StreamError::failed("sending", e)),
    }
}

/// Receives a file of `size` bytes into `part`; more or fewer bytes than
/// announced is a failure.
pub async fn receive_file(
    mut stream: FileReceiver,
    mut part: PartFile,
    size: u64,
    mut cancel: watch::Receiver<bool>,
    progress: &AtomicU64,
) -> Result<PathBuf, StreamError> {
    loop {
        let read = tokio::select! {
            () = cancelled(&mut cancel) => {
                stream.cancel();
                return Err(StreamError::Cancelled);
            }
            read = stream.read() => read.map_err(|e| StreamError::failed("receiving", e))?,
        };
        let Some(bytes) = read else { break };
        if part.len() + bytes.len() as u64 > size {
            stream.cancel();
            return Err(StreamError::Failed("more data than announced".into()));
        }
        part.write(&bytes)
            .await
            .map_err(|e| StreamError::failed("writing", e))?;
        progress.fetch_add(bytes.len() as u64, Ordering::Relaxed);
    }
    if part.len() != size {
        return Err(StreamError::Failed("the file arrived incomplete".into()));
    }
    part.commit()
        .await
        .map_err(|e| StreamError::failed("saving", e))
}

/// Passes a pasted file of `size` bytes on to Explorer through `chunks`.
/// Explorer letting go of the file (the receiver dropped) cancels it.
pub async fn forward_file(
    mut stream: FileReceiver,
    chunks: mpsc::Sender<Chunk>,
    size: u64,
    mut cancel: watch::Receiver<bool>,
    progress: &AtomicU64,
) -> Result<(), StreamError> {
    let mut received = 0u64;
    loop {
        let read = tokio::select! {
            () = cancelled(&mut cancel) => {
                stream.cancel();
                return Err(StreamError::Cancelled);
            }
            read = stream.read() => read.map_err(|e| StreamError::failed("receiving", e))?,
        };
        let Some(bytes) = read else { break };
        received += bytes.len() as u64;
        if received > size {
            stream.cancel();
            return Err(StreamError::Failed("more data than announced".into()));
        }
        let count = bytes.len() as u64;
        let delivered = tokio::select! {
            () = cancelled(&mut cancel) => Err(()),
            sent = chunks.send(Chunk::Data(bytes)) => sent.map_err(|_| ()),
        };
        if delivered.is_err() {
            stream.cancel();
            return Err(StreamError::Cancelled);
        }
        progress.fetch_add(count, Ordering::Relaxed);
    }
    if received != size {
        return Err(StreamError::Failed("the file arrived incomplete".into()));
    }
    chunks
        .send(Chunk::End)
        .await
        .map_err(|_| StreamError::Cancelled)
}

/// A file read in chunks, up to the size it was listed with.
struct SourceFile {
    file: Option<File>,
    remaining: u64,
}

impl SourceFile {
    async fn open(path: PathBuf, size: u64) -> Result<Self, StreamError> {
        let file = blocking(move || File::open(path))
            .await
            .map_err(|e| StreamError::failed("opening the file", e))?;
        Ok(Self {
            file: Some(file),
            remaining: size,
        })
    }

    /// The next chunk; `None` once the listed size was read. A file that
    /// got shorter since it was listed is an error.
    async fn next(&mut self) -> Result<Option<Vec<u8>>, StreamError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let mut file = self
            .file
            .take()
            .ok_or_else(|| StreamError::Failed("the file was closed".into()))?;
        let want = usize::try_from(self.remaining.min(READ_CHUNK as u64)).unwrap_or(READ_CHUNK);
        let (file, chunk) = blocking(move || {
            let mut chunk = vec![0; want];
            let read = file.read(&mut chunk)?;
            chunk.truncate(read);
            Ok((file, chunk))
        })
        .await
        .map_err(|e| StreamError::failed("reading the file", e))?;
        if chunk.is_empty() {
            return Err(StreamError::Failed("the file got shorter".into()));
        }
        self.remaining -= chunk.len() as u64;
        self.file = Some(file);
        Ok(Some(chunk))
    }
}
