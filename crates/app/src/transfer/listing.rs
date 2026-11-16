//! What this PC sends: the files and folders the user picked, dropped or
//! copied, walked into the list the other PC receives. Folders come before
//! what they hold. Links and junctions are not followed (a loop, or a
//! folder outside what was picked); they are left out.
//!
//! Blocking (it reads the disk): call it off the runtime's workers.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use protocol::transfer::{FileEntry, MAX_TRANSFER_ENTRIES, list_fits};

use super::names::unique_name;
use super::safe_path::{PathError, SafePath};

/// The list to send, with where each entry is on this PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<FileEntry>,
    /// `sources[i]` is the local path of `entries[i]`.
    pub sources: Vec<PathBuf>,
}

impl Listing {
    /// The local path and size of the file at `index`, if it is one.
    pub fn file(&self, index: u32) -> Option<(&Path, &FileEntry)> {
        let index = usize::try_from(index).ok()?;
        let entry = self.entries.get(index).filter(|e| !e.is_dir)?;
        Some((&self.sources[index], entry))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListingError {
    Nothing,
    TooMany,
    /// A name the other PC could not create, or a path too deep.
    Unsendable {
        path: PathBuf,
        reason: PathError,
    },
    /// The disk could not be read there; `detail` is the I/O error.
    Unreadable {
        path: PathBuf,
        detail: String,
    },
    /// A picked path without a name of its own (a drive, `..`).
    Unnamed(PathBuf),
}

/// The English text, for the terminal (the Italian one is in
/// `crate::text`).
impl fmt::Display for ListingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nothing => f.write_str("nothing to send"),
            Self::TooMany => write!(f, "more than {MAX_TRANSFER_ENTRIES} files and folders"),
            Self::Unsendable { path, reason } => write!(f, "{}: {reason}", path.display()),
            Self::Unreadable { path, detail } => write!(f, "{}: {detail}", path.display()),
            Self::Unnamed(path) => write!(f, "{}: not a file or folder", path.display()),
        }
    }
}

impl std::error::Error for ListingError {}

/// Walks `picked` into a list. Two picked items with the same name (from
/// different folders) keep both: the second becomes `name (1)`.
pub fn list(picked: &[PathBuf]) -> Result<Listing, ListingError> {
    let mut listing = Listing {
        entries: Vec::new(),
        sources: Vec::new(),
    };
    let mut roots: Vec<String> = Vec::new();
    for path in picked {
        let metadata = read_metadata(path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| ListingError::Unnamed(path.clone()))?;
        let name = unique_name(&name, metadata.is_dir(), |n| roots.iter().any(|r| r == n))
            .ok_or(ListingError::TooMany)?;
        let root = SafePath::name(&name).map_err(|reason| unsendable(path, reason))?;
        roots.push(name);
        add(&mut listing, path, &root, &metadata)?;
    }
    if listing.entries.is_empty() {
        return Err(ListingError::Nothing);
    }
    if !list_fits(&listing.entries) {
        return Err(ListingError::TooMany);
    }
    Ok(listing)
}

/// Adds `path` (listed as `relative`) and, for a folder, what it holds.
fn add(
    listing: &mut Listing,
    path: &Path,
    relative: &SafePath,
    metadata: &fs::Metadata,
) -> Result<(), ListingError> {
    if listing.entries.len() >= MAX_TRANSFER_ENTRIES {
        return Err(ListingError::TooMany);
    }
    let entry = if metadata.is_dir() {
        FileEntry::folder(relative.to_wire())
    } else {
        FileEntry::file(relative.to_wire(), metadata.len())
    };
    listing.entries.push(entry);
    listing.sources.push(path.to_path_buf());
    if !metadata.is_dir() {
        return Ok(());
    }
    let mut children: Vec<PathBuf> = fs::read_dir(path)
        .map_err(|e| unreadable(path, &e))?
        .map(|child| child.map(|c| c.path()).map_err(|e| unreadable(path, &e)))
        .collect::<Result<_, _>>()?;
    children.sort();
    for child in children {
        let metadata = read_metadata(&child)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let name = child.file_name().unwrap_or_default().to_string_lossy();
        let child_relative = SafePath::parse(&format!("{}/{name}", relative.to_wire()))
            .map_err(|reason| unsendable(&child, reason))?;
        add(listing, &child, &child_relative, &metadata)?;
    }
    Ok(())
}

/// The item's own metadata: a link is reported as a link, not followed.
fn read_metadata(path: &Path) -> Result<fs::Metadata, ListingError> {
    fs::symlink_metadata(path).map_err(|e| unreadable(path, &e))
}

fn unreadable(path: &Path, error: &std::io::Error) -> ListingError {
    ListingError::Unreadable {
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

fn unsendable(path: &Path, reason: PathError) -> ListingError {
    ListingError::Unsendable {
        path: path.to_path_buf(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Foto");
        fs::create_dir_all(root.join("2024")).unwrap();
        fs::write(root.join("b.jpg"), [1; 3]).unwrap();
        fs::write(root.join("2024").join("a.jpg"), [1; 5]).unwrap();
        fs::write(dir.path().join("nota.txt"), "ciao").unwrap();
        dir
    }

    #[test]
    fn folders_are_walked_with_their_structure() {
        let dir = tree();
        let listing = list(&[dir.path().join("Foto"), dir.path().join("nota.txt")]).unwrap();
        assert_eq!(
            listing.entries,
            [
                FileEntry::folder("Foto"),
                FileEntry::folder("Foto/2024"),
                FileEntry::file("Foto/2024/a.jpg", 5),
                FileEntry::file("Foto/b.jpg", 3),
                FileEntry::file("nota.txt", 4),
            ]
        );
        assert_eq!(listing.sources[2], dir.path().join("Foto/2024/a.jpg"));
        let (source, entry) = listing.file(4).unwrap();
        assert_eq!(
            (source, entry.size),
            (dir.path().join("nota.txt").as_path(), 4)
        );
        assert!(listing.file(0).is_none(), "a folder is not a file");
        assert!(listing.file(99).is_none());
    }

    #[test]
    fn two_picked_items_with_the_same_name_are_both_kept() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::write(a.path().join("x.txt"), "1").unwrap();
        fs::write(b.path().join("x.txt"), "22").unwrap();
        let listing = list(&[a.path().join("x.txt"), b.path().join("x.txt")]).unwrap();
        assert_eq!(
            listing.entries,
            [FileEntry::file("x.txt", 1), FileEntry::file("x (1).txt", 2)]
        );
    }

    #[test]
    fn nothing_picked_and_missing_files_are_errors() {
        assert_eq!(list(&[]), Err(ListingError::Nothing));
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            list(&[dir.path().join("assente")]),
            Err(ListingError::Unreadable { .. })
        ));
    }

    #[test]
    fn a_path_without_a_name_is_not_sendable() {
        let dir = tempfile::tempdir().unwrap();
        let unnamed = dir.path().join("..");
        assert_eq!(
            list(std::slice::from_ref(&unnamed)),
            Err(ListingError::Unnamed(unnamed))
        );
    }

    #[test]
    fn an_empty_folder_is_sent_as_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("vuota")).unwrap();
        let listing = list(&[dir.path().join("vuota")]).unwrap();
        assert_eq!(listing.entries, [FileEntry::folder("vuota")]);
    }

    #[test]
    fn too_many_entries_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("molti");
        fs::create_dir(&root).unwrap();
        for i in 0..MAX_TRANSFER_ENTRIES {
            fs::write(root.join(format!("{i}")), "").unwrap();
        }
        assert_eq!(list(&[root]), Err(ListingError::TooMany));
    }
}
