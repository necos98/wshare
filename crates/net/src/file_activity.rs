//! Which file streams a connection carries, so the host can tell the
//! congestion a file transfer causes from the video's own (see
//! [`crate::TransportStats::file_streams_open`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The file streams of one connection, both directions. Cheap to clone:
/// the file channel counts, the video channel reads.
#[derive(Debug, Clone, Default)]
pub(crate) struct FileActivity(Arc<Counts>);

#[derive(Debug, Default)]
struct Counts {
    open: AtomicU64,
    opened: AtomicU64,
}

impl FileActivity {
    /// Counts a stream as open until the returned guard is dropped.
    pub(crate) fn track(&self) -> OpenFileStream {
        self.0.open.fetch_add(1, Ordering::Relaxed);
        self.0.opened.fetch_add(1, Ordering::Relaxed);
        OpenFileStream(self.0.clone())
    }

    /// File streams open now.
    pub(crate) fn open(&self) -> u64 {
        self.0.open.load(Ordering::Relaxed)
    }

    /// File streams opened since the connection started, those over
    /// included.
    pub(crate) fn opened(&self) -> u64 {
        self.0.opened.load(Ordering::Relaxed)
    }
}

/// One open file stream: counted by its [`FileActivity`] while it lives.
#[derive(Debug)]
pub(crate) struct OpenFileStream(Arc<Counts>);

impl Drop for OpenFileStream {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_counts_as_open_while_its_guard_lives() {
        let activity = FileActivity::default();
        let reader = activity.clone();
        assert_eq!((reader.open(), reader.opened()), (0, 0));
        let first = activity.track();
        let second = activity.track();
        assert_eq!((reader.open(), reader.opened()), (2, 2));
        drop(first);
        assert_eq!((reader.open(), reader.opened()), (1, 2));
        drop(second);
        assert_eq!((reader.open(), reader.opened()), (0, 2));
    }
}
