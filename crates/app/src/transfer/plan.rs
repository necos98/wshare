//! Where a pushed list lands: every entry checked (see [`SafePath`]), every
//! item the sender picked given a free name in the destination folder, and
//! each entry mapped to its place there. Nothing outside the destination is
//! ever named.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use protocol::transfer::{FileEntry, list_fits};

use super::names::unique_name;
use super::part_file::part_path;
use super::safe_path::{PathError, SafePath};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    Empty,
    TooMany,
    BadPath {
        path: String,
        reason: PathError,
    },
    /// The same path twice.
    Duplicate(String),
    /// An entry whose folder is not listed before it, or a folder with a
    /// size.
    Malformed(String),
    /// Every numbered name of an item is taken.
    NoFreeName(String),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty list"),
            Self::TooMany => f.write_str("too many files"),
            Self::BadPath { path, reason } => write!(f, "{path:?}: {reason}"),
            Self::Duplicate(path) => write!(f, "{path:?} listed twice"),
            Self::Malformed(path) => write!(f, "{path:?} does not fit the list"),
            Self::NoFreeName(name) => write!(f, "no free name for {name:?}"),
        }
    }
}

impl std::error::Error for PlanError {}

/// One entry's place on this PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub target: PathBuf,
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivePlan {
    destination: PathBuf,
    placed: Vec<Placed>,
    /// The items picked by the sender, under their names here.
    roots: Vec<PathBuf>,
}

impl ReceivePlan {
    /// Plans `entries` into `destination`. `taken` says whether a path is
    /// in use (on disk, or reserved by a transfer still arriving).
    pub fn new(
        destination: &Path,
        entries: &[FileEntry],
        taken: impl Fn(&Path) -> bool,
    ) -> Result<Self, PlanError> {
        let paths = check_list(entries)?;
        let mut renamed: Vec<(String, String)> = Vec::new();
        let mut placed = Vec::with_capacity(entries.len());
        let mut roots = Vec::new();
        for (entry, path) in entries.iter().zip(&paths) {
            let original = path.root().to_string();
            let root = match renamed.iter().find(|(from, _)| *from == original) {
                Some((_, to)) => to.clone(),
                None => {
                    let free = unique_name(&original, entry.is_dir, |name| {
                        let candidate = destination.join(name);
                        taken(&candidate)
                            || taken(&part_path(&candidate))
                            || renamed.iter().any(|(_, to)| to == name)
                    })
                    .ok_or_else(|| PlanError::NoFreeName(original.clone()))?;
                    roots.push(destination.join(&free));
                    renamed.push((original, free.clone()));
                    free
                }
            };
            let target = path.with_root(&root).under(destination);
            debug_assert!(target.starts_with(destination));
            placed.push(Placed {
                target,
                size: entry.size,
                is_dir: entry.is_dir,
            });
        }
        Ok(Self {
            destination: destination.to_path_buf(),
            placed,
            roots,
        })
    }

    pub fn destination(&self) -> &Path {
        &self.destination
    }

    #[cfg(test)]
    pub fn entries(&self) -> &[Placed] {
        &self.placed
    }

    /// The entry at `index`, if it is a file.
    pub fn file(&self, index: u32) -> Option<&Placed> {
        let index = usize::try_from(index).ok()?;
        self.placed.get(index).filter(|p| !p.is_dir)
    }

    /// The items picked by the sender, as named here.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn file_indexes(&self) -> impl Iterator<Item = u32> + '_ {
        (0u32..)
            .zip(&self.placed)
            .filter(|(_, p)| !p.is_dir)
            .map(|(index, _)| index)
    }

    /// Creates the destination and every folder of the list, parents first;
    /// returns those created, for [`remove_created`]. Blocking.
    pub fn create_folders(&self) -> std::io::Result<Vec<PathBuf>> {
        std::fs::create_dir_all(&self.destination)?;
        let mut created = Vec::new();
        for folder in self.placed.iter().filter(|p| p.is_dir) {
            match std::fs::create_dir(&folder.target) {
                Ok(()) => created.push(folder.target.clone()),
                Err(e) => {
                    remove_created(&created);
                    return Err(e);
                }
            }
        }
        Ok(created)
    }
}

/// Removes the folders in `created` that stayed empty, deepest first: what
/// a transfer that did not complete leaves of the structure. Blocking.
pub fn remove_created(created: &[PathBuf]) {
    for folder in created.iter().rev() {
        // Fails when not empty (files that did arrive): kept then.
        let _ = std::fs::remove_dir(folder);
    }
}

/// Every path of a received list, checked: not empty, not too many, each
/// path valid, each folder listed before what it holds and only once,
/// folders without a size.
pub fn check_list(entries: &[FileEntry]) -> Result<Vec<SafePath>, PlanError> {
    if entries.is_empty() {
        return Err(PlanError::Empty);
    }
    if !list_fits(entries) {
        return Err(PlanError::TooMany);
    }
    check_structure(entries)
}

fn check_structure(entries: &[FileEntry]) -> Result<Vec<SafePath>, PlanError> {
    let mut seen: HashSet<String> = HashSet::with_capacity(entries.len());
    let mut folders: HashSet<String> = HashSet::new();
    let mut paths = Vec::with_capacity(entries.len());
    for entry in entries {
        let path = SafePath::parse(&entry.path).map_err(|reason| PlanError::BadPath {
            path: entry.path.clone(),
            reason,
        })?;
        let wire = path.to_wire();
        // Windows names ignore case: `A` and `a` are the same file.
        if !seen.insert(wire.to_lowercase()) {
            return Err(PlanError::Duplicate(wire));
        }
        let parent_listed = path
            .parent()
            .is_none_or(|parent| folders.contains(&parent.to_wire().to_lowercase()));
        if !parent_listed || (entry.is_dir && entry.size != 0) {
            return Err(PlanError::Malformed(wire));
        }
        if entry.is_dir {
            folders.insert(wire.to_lowercase());
        }
        paths.push(path);
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nothing_taken(_: &Path) -> bool {
        false
    }

    fn dest() -> PathBuf {
        PathBuf::from("C:\\Users\\u\\Downloads\\wshare")
    }

    #[test]
    fn entries_land_under_the_destination_with_their_structure() {
        let entries = [
            FileEntry::folder("Foto"),
            FileEntry::file("Foto/a.jpg", 5),
            FileEntry::file("nota.txt", 2),
        ];
        let plan = ReceivePlan::new(&dest(), &entries, nothing_taken).unwrap();
        let targets: Vec<&Path> = plan.entries().iter().map(|p| p.target.as_path()).collect();
        assert_eq!(
            targets,
            [
                dest().join("Foto"),
                dest().join("Foto").join("a.jpg"),
                dest().join("nota.txt")
            ]
        );
        assert_eq!(plan.roots(), [dest().join("Foto"), dest().join("nota.txt")]);
        assert_eq!(plan.file_indexes().collect::<Vec<_>>(), [1, 2]);
        assert!(plan.file(0).is_none());
        assert_eq!(plan.file(2).unwrap().size, 2);
    }

    #[test]
    fn taken_names_are_numbered_for_the_whole_item() {
        let taken = |path: &Path| {
            path == dest().join("Foto")
                || path == dest().join("nota.txt")
                || path == dest().join("altro.txt.part")
        };
        let entries = [
            FileEntry::folder("Foto"),
            FileEntry::file("Foto/a.jpg", 5),
            FileEntry::file("nota.txt", 2),
            FileEntry::file("altro.txt", 2),
        ];
        let plan = ReceivePlan::new(&dest(), &entries, taken).unwrap();
        let targets: Vec<PathBuf> = plan.entries().iter().map(|p| p.target.clone()).collect();
        assert_eq!(
            targets,
            [
                dest().join("Foto (1)"),
                dest().join("Foto (1)").join("a.jpg"),
                dest().join("nota (1).txt"),
                dest().join("altro (1).txt"),
            ]
        );
    }

    #[test]
    fn dangerous_paths_refuse_the_whole_list() {
        for path in [
            "../x",
            "C:/Windows/x",
            "a/../../b",
            "NUL",
            "a:b",
            "/x",
            "a\\..\\b",
        ] {
            let entries = [FileEntry::file(path, 1)];
            assert!(
                matches!(
                    ReceivePlan::new(&dest(), &entries, nothing_taken),
                    Err(PlanError::BadPath { .. })
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn the_structure_must_hold_together() {
        let orphan = [FileEntry::file("Foto/a.jpg", 1)];
        assert_eq!(
            ReceivePlan::new(&dest(), &orphan, nothing_taken),
            Err(PlanError::Malformed("Foto/a.jpg".into()))
        );
        let sized_folder = [FileEntry {
            path: "d".into(),
            size: 3,
            is_dir: true,
        }];
        assert!(ReceivePlan::new(&dest(), &sized_folder, nothing_taken).is_err());
        let file_as_folder = [FileEntry::file("d", 1), FileEntry::file("d/x", 1)];
        assert!(ReceivePlan::new(&dest(), &file_as_folder, nothing_taken).is_err());
        let twice = [FileEntry::file("a.txt", 1), FileEntry::file("A.TXT", 1)];
        assert_eq!(
            ReceivePlan::new(&dest(), &twice, nothing_taken),
            Err(PlanError::Duplicate("A.TXT".into()))
        );
        assert_eq!(
            ReceivePlan::new(&dest(), &[], nothing_taken),
            Err(PlanError::Empty)
        );
    }

    #[test]
    fn folders_are_created_and_empty_ones_removed_after_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("wshare");
        let entries = [
            FileEntry::folder("A"),
            FileEntry::folder("A/B"),
            FileEntry::file("A/B/f", 1),
            FileEntry::folder("C"),
        ];
        let plan = ReceivePlan::new(&destination, &entries, |p| p.exists()).unwrap();
        let created = plan.create_folders().unwrap();
        assert_eq!(created.len(), 3);
        assert!(destination.join("A").join("B").is_dir());
        // A file arrived in A, nothing else did.
        std::fs::write(destination.join("A").join("kept"), "x").unwrap();
        remove_created(&created);
        assert!(destination.join("A").join("kept").exists());
        assert!(!destination.join("A").join("B").exists());
        assert!(!destination.join("C").exists());
    }
}
