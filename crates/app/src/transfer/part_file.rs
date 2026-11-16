//! A file being received: written as `name.part` next to where it belongs,
//! renamed to its name only once complete. Dropped before that (cancelled,
//! failed, the session gone), the `.part` file is deleted.
//!
//! Disk writes run on tokio's blocking pool, never on a runtime worker.
//! The file is opened with delete sharing, so it can be deleted even while
//! a write still holds it.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::free_rename::rename_to_free_name;

/// Appended to the name of a file until it is complete.
pub const PART_SUFFIX: &str = ".part";

/// Bytes gathered before one write to disk.
const WRITE_BUFFER: usize = 1024 * 1024;

const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const FILE_SHARE_DELETE: u32 = 0x4;

/// `target` with [`PART_SUFFIX`] appended to its name.
pub fn part_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(PART_SUFFIX);
    target.with_file_name(name)
}

pub struct PartFile {
    part: PathBuf,
    target: PathBuf,
    /// `None` only while a write holds it, or once committed.
    file: Option<File>,
    buffer: Vec<u8>,
    written: u64,
    committed: bool,
}

impl PartFile {
    /// Creates `target.part`; fails if it exists (never replaces a file).
    ///
    /// The `PartFile` is built on the disk thread, together with the file:
    /// if the caller is cancelled before the creation ends, the result is
    /// dropped there and its `Drop` deletes the file.
    pub async fn create(target: PathBuf) -> io::Result<Self> {
        blocking(move || Self::create_now(target)).await
    }

    fn create_now(target: PathBuf) -> io::Result<Self> {
        let part = part_path(&target);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&part)?;
        Ok(Self {
            part,
            target,
            file: Some(file),
            buffer: Vec::with_capacity(WRITE_BUFFER),
            written: 0,
            committed: false,
        })
    }

    /// Bytes received so far.
    pub fn len(&self) -> u64 {
        self.written + self.buffer.len() as u64
    }

    pub async fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() >= WRITE_BUFFER {
            self.flush().await?;
        }
        Ok(())
    }

    /// Writes what is left, closes the file and gives it its name: the
    /// target's, or the next free one if a file of that name appeared
    /// meanwhile. Returns where it is.
    pub async fn commit(mut self) -> io::Result<PathBuf> {
        self.flush().await?;
        let file = self.file.take().ok_or_else(closed)?;
        let (part, target) = (self.part.clone(), self.target.clone());
        let placed = blocking(move || {
            file.sync_all()?;
            drop(file);
            rename_to_free_name(&part, &target)
        })
        .await?;
        self.committed = true;
        Ok(placed)
    }

    async fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let mut file = self.file.take().ok_or_else(closed)?;
        let bytes = std::mem::replace(&mut self.buffer, Vec::with_capacity(WRITE_BUFFER));
        let count = bytes.len() as u64;
        let file = blocking(move || file.write_all(&bytes).map(|()| file)).await?;
        self.file = Some(file);
        self.written += count;
        Ok(())
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        // Closed first: the delete then takes effect at once.
        drop(self.file.take());
        // Already gone is fine; anything else cannot be helped from here.
        let _ = std::fs::remove_file(&self.part);
    }
}

fn closed() -> io::Error {
    io::Error::other("the file was closed after an earlier failure")
}

pub(super) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|e| Err(io::Error::other(format!("disk task failed: {e}"))))
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Waker};

    use super::*;

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn a_complete_file_takes_its_name_and_leaves_no_part() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = PartFile::create(dir.path().join("a.txt")).await.unwrap();
        assert_eq!(entries(dir.path()), ["a.txt.part"]);
        file.write(b"ciao ").await.unwrap();
        file.write(&vec![7; WRITE_BUFFER + 1]).await.unwrap();
        assert_eq!(file.len(), 5 + WRITE_BUFFER as u64 + 1);
        let placed = file.commit().await.unwrap();
        assert_eq!(placed, dir.path().join("a.txt"));
        assert_eq!(entries(dir.path()), ["a.txt"]);
        assert_eq!(
            std::fs::metadata(&placed).unwrap().len(),
            6 + WRITE_BUFFER as u64
        );
    }

    #[tokio::test]
    async fn a_dropped_file_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = PartFile::create(dir.path().join("big.bin")).await.unwrap();
        file.write(&vec![1; WRITE_BUFFER * 2]).await.unwrap();
        drop(file);
        assert!(entries(dir.path()).is_empty());
    }

    /// Runs `work` on a runtime of its own with `disk_threads` blocking
    /// threads, then shuts it down, which waits for the disk work still
    /// running: what is on disk afterwards is final.
    fn run_to_the_end(disk_threads: usize, work: impl Future<Output = ()>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(disk_threads)
            .build()
            .unwrap();
        runtime.block_on(work);
        drop(runtime);
    }

    #[test]
    fn a_file_cancelled_mid_write_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("big.bin");
        run_to_the_end(1, async move {
            let (written, first_write) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let mut file = PartFile::create(target).await.unwrap();
                file.write(&vec![1; WRITE_BUFFER]).await.unwrap();
                written.send(()).unwrap();
                loop {
                    file.write(&vec![1; WRITE_BUFFER]).await.unwrap();
                }
            });
            first_write.await.unwrap();
            // The task now waits on its next write to disk, which the
            // shutdown lets finish: its handle closes only then.
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        });
        assert!(entries(dir.path()).is_empty(), "{:?}", entries(dir.path()));
    }

    #[test]
    fn a_file_cancelled_while_being_created_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("big.bin");
        run_to_the_end(1, async move {
            // The only disk thread waits for `release`: the creation is
            // queued behind it and runs only once its caller is gone.
            let (release, gate) = std::sync::mpsc::channel::<()>();
            let busy = tokio::task::spawn_blocking(move || gate.recv());
            let mut create = Box::pin(PartFile::create(target));
            let mut cx = Context::from_waker(Waker::noop());
            assert!(create.as_mut().poll(&mut cx).is_pending());
            drop(create);
            release.send(()).unwrap();
            busy.await.unwrap().unwrap();
            // Queued after the creation on the same single thread: once
            // this has run, so has the creation.
            tokio::task::spawn_blocking(|| ()).await.unwrap();
        });
        assert!(entries(dir.path()).is_empty(), "{:?}", entries(dir.path()));
    }

    #[tokio::test]
    async fn an_existing_part_or_target_is_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt.part"), "mio").unwrap();
        assert!(PartFile::create(dir.path().join("a.txt")).await.is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt.part")).unwrap(),
            "mio"
        );

        let file = PartFile::create(dir.path().join("b.txt")).await.unwrap();
        std::fs::write(dir.path().join("b.txt"), "arrivato prima").unwrap();
        let placed = file.commit().await.unwrap();
        assert_eq!(placed, dir.path().join("b (1).txt"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "arrivato prima"
        );
    }

    #[test]
    fn the_part_name_is_the_name_plus_the_suffix() {
        assert_eq!(
            part_path(Path::new("C:\\x\\foto.jpg")),
            PathBuf::from("C:\\x\\foto.jpg.part")
        );
    }
}
