//! A pushed transfer this PC accepted: where its files go, which arrived,
//! and the folders it created. If it does not complete, the folders left
//! empty are removed once the last of its tasks lets go of it (the `.part`
//! files are gone by then, see [`super::part_file`]).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use protocol::transfer::FileEntry;

use super::plan::{Placed, ReceivePlan, remove_created};

pub struct Receiving {
    plan: ReceivePlan,
    /// The entries as the sender listed them, to check stream headers.
    entries: Vec<FileEntry>,
    created: Vec<PathBuf>,
    started: Mutex<HashSet<u32>>,
    remaining: AtomicUsize,
    completed: AtomicBool,
}

impl Receiving {
    pub fn new(plan: ReceivePlan, entries: Vec<FileEntry>, created: Vec<PathBuf>) -> Self {
        let remaining = plan.file_indexes().count();
        Self {
            plan,
            entries,
            created,
            started: Mutex::new(HashSet::new()),
            remaining: AtomicUsize::new(remaining),
            completed: AtomicBool::new(false),
        }
    }

    pub fn plan(&self) -> &ReceivePlan {
        &self.plan
    }

    /// Where the file a stream announces goes, if the stream is one this
    /// transfer expects: a file of the list, announced as listed, not
    /// arrived or arriving already.
    pub fn claim(&self, index: u32, announced: &FileEntry) -> Option<Placed> {
        let placed = self.plan.file(index)?;
        let listed = self.entries.get(usize::try_from(index).ok()?)?;
        if listed != announced {
            return None;
        }
        let mut started = self.started.lock().unwrap_or_else(|e| e.into_inner());
        started.insert(index).then(|| placed.clone())
    }

    /// One more file arrived whole; true when it was the last one.
    pub fn file_done(&self) -> bool {
        let last = self.remaining.fetch_sub(1, Ordering::AcqRel) == 1;
        if last {
            self.completed.store(true, Ordering::Release);
        }
        last
    }

    /// Whether the list holds no file at all (only folders).
    pub fn has_no_files(&self) -> bool {
        self.remaining.load(Ordering::Acquire) == 0
    }

    /// Everything arrived (or there was nothing to wait for): what was
    /// created stays.
    pub fn mark_complete(&self) {
        self.completed.store(true, Ordering::Release);
    }
}

impl Drop for Receiving {
    fn drop(&mut self) {
        if !self.completed.load(Ordering::Acquire) {
            remove_created(&self.created);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiving(dir: &std::path::Path) -> Receiving {
        let entries = vec![
            FileEntry::folder("D"),
            FileEntry::file("D/a", 1),
            FileEntry::file("b", 2),
        ];
        let plan = ReceivePlan::new(dir, &entries, |p| p.exists()).unwrap();
        let created = plan.create_folders().unwrap();
        Receiving::new(plan, entries, created)
    }

    #[test]
    fn a_stream_is_claimed_once_and_only_as_listed() {
        let dir = tempfile::tempdir().unwrap();
        let receiving = receiving(dir.path());
        assert!(
            receiving.claim(0, &FileEntry::folder("D")).is_none(),
            "a folder"
        );
        assert!(receiving.claim(1, &FileEntry::file("D/a", 99)).is_none());
        assert!(receiving.claim(1, &FileEntry::file("b", 2)).is_none());
        let placed = receiving.claim(1, &FileEntry::file("D/a", 1)).unwrap();
        assert_eq!(placed.target, dir.path().join("D").join("a"));
        assert!(
            receiving.claim(1, &FileEntry::file("D/a", 1)).is_none(),
            "twice"
        );
        assert!(receiving.claim(7, &FileEntry::file("D/a", 1)).is_none());
    }

    #[test]
    fn the_last_file_completes_and_keeps_the_folders() {
        let dir = tempfile::tempdir().unwrap();
        let receiving = receiving(dir.path());
        assert!(!receiving.has_no_files());
        assert!(!receiving.file_done());
        assert!(receiving.file_done());
        drop(receiving);
        assert!(dir.path().join("D").is_dir());
    }

    #[test]
    fn an_incomplete_transfer_removes_its_empty_folders() {
        let dir = tempfile::tempdir().unwrap();
        let receiving = receiving(dir.path());
        assert!(dir.path().join("D").is_dir());
        drop(receiving);
        assert!(!dir.path().join("D").exists());
    }
}
