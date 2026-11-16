//! Gives a finished file its name: the one it was sent with, or the next
//! free `nome (1).ext` when that is taken. Never replaces a file, not even
//! one that appears between choosing a name and renaming to it: the rename
//! itself refuses a taken name, and the next free one is tried.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf, Prefix};

use windows::Win32::Storage::FileSystem::{MOVE_FILE_FLAGS, MoveFileExW};
use windows::core::{HRESULT, HSTRING};

use super::names::unique_name;

/// Names that may be taken between choosing one and renaming to it before
/// giving up: each one lost is skipped on the next attempt.
const MAX_ATTEMPTS: u32 = 16;

/// Turns an absolute path into one Win32 takes beyond MAX_PATH.
const VERBATIM: &str = r"\\?\";
/// The same for a network path, whose leading `\` it replaces.
const VERBATIM_UNC: &str = r"\\?\UNC";

/// An `HRESULT_FROM_WIN32` keeps the Win32 error in its low word.
const WIN32_CODE_MASK: u32 = 0xFFFF;

/// Renames `from` to `target`, or to the first free `name (n).ext` next to
/// it; returns where it is.
pub(super) fn rename_to_free_name(from: &Path, target: &Path) -> io::Result<PathBuf> {
    rename_checking(from, target, |path| path.exists())
}

/// [`rename_to_free_name`], asking `exists` which names are taken.
fn rename_checking(
    from: &Path,
    target: &Path,
    exists: impl Fn(&Path) -> bool,
) -> io::Result<PathBuf> {
    for _ in 0..MAX_ATTEMPTS {
        let candidate = free_target(target, &exists)?;
        match rename_no_replace(from, &candidate) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            renamed => return renamed.map(|()| candidate),
        }
    }
    Err(no_free_name())
}

/// `target`, or the first `name (n).ext` next to it that `exists` denies.
fn free_target(target: &Path, exists: &impl Fn(&Path) -> bool) -> io::Result<PathBuf> {
    // Checked as it is first: a free name is kept even if not valid UTF-8.
    if !exists(target) {
        return Ok(target.to_path_buf());
    }
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let folder = target.parent().unwrap_or(Path::new(""));
    unique_name(&name, false, |candidate| exists(&folder.join(candidate)))
        .map(|name| folder.join(name))
        .ok_or_else(no_free_name)
}

/// Renames `from` to `to`; fails with [`io::ErrorKind::AlreadyExists`]
/// if `to` exists.
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (long_path(from)?, long_path(to)?);
    // SAFETY: both strings outlive the call. Without
    // MOVEFILE_REPLACE_EXISTING the rename fails on a taken name.
    unsafe { MoveFileExW(&from, &to, MOVE_FILE_FLAGS::default()) }.map_err(io_error)
}

/// `path` made absolute, in the `\\?\` form, as std's own file calls use
/// it: a received file may lie deeper than MAX_PATH.
fn long_path(path: &Path) -> io::Result<HSTRING> {
    let full = std::path::absolute(path)?;
    let wide = full.as_os_str().encode_wide();
    let long: Vec<u16> = match full.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(_) => VERBATIM.encode_utf16().chain(wide).collect(),
            // `\\server\share` becomes `\\?\UNC\server\share`.
            Prefix::UNC(..) => VERBATIM_UNC.encode_utf16().chain(wide.skip(1)).collect(),
            // Already verbatim, or a device path: taken as it is.
            _ => wide.collect(),
        },
        _ => wide.collect(),
    };
    Ok(HSTRING::from_wide(&long))
}

/// The `io::Error` std gives for the Win32 error behind `error`, so its
/// kind (`AlreadyExists`...) is the usual one.
fn io_error(error: windows::core::Error) -> io::Error {
    let code = error.code();
    let win32 = code.0 as u32 & WIN32_CODE_MASK;
    if HRESULT::from_win32(win32) == code {
        io::Error::from_raw_os_error(win32 as i32)
    } else {
        io::Error::other(error)
    }
}

fn no_free_name() -> io::Error {
    io::Error::new(io::ErrorKind::AlreadyExists, "no free name")
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn a_free_name_is_taken_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("a.txt.part");
        std::fs::write(&from, "nuovo").unwrap();
        let placed = rename_to_free_name(&from, &dir.path().join("a.txt")).unwrap();
        assert_eq!(placed, dir.path().join("a.txt"));
        assert_eq!(read(&placed), "nuovo");
        assert!(!from.exists());
    }

    #[test]
    fn a_taken_name_gets_the_next_free_one() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("a.txt.part");
        std::fs::write(&from, "nuovo").unwrap();
        std::fs::write(dir.path().join("a.txt"), "vecchio").unwrap();
        let placed = rename_to_free_name(&from, &dir.path().join("a.txt")).unwrap();
        assert_eq!(placed, dir.path().join("a (1).txt"));
        assert_eq!(read(&placed), "nuovo");
        assert_eq!(read(&dir.path().join("a.txt")), "vecchio");
    }

    #[test]
    fn a_file_appearing_after_the_check_is_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("b.txt.part");
        std::fs::write(&from, "ricevuto").unwrap();
        std::fs::write(dir.path().join("b.txt"), "arrivato prima").unwrap();
        // The first check misses `b.txt`, as if it appeared right after.
        let checks = Cell::new(0);
        let exists = |path: &Path| {
            checks.set(checks.get() + 1);
            checks.get() > 1 && path.exists()
        };
        let placed = rename_checking(&from, &dir.path().join("b.txt"), exists).unwrap();
        assert_eq!(placed, dir.path().join("b (1).txt"));
        assert_eq!(read(&placed), "ricevuto");
        assert_eq!(read(&dir.path().join("b.txt")), "arrivato prima");
    }

    #[test]
    fn a_name_taken_at_every_attempt_fails_without_replacing() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("c.txt.part");
        std::fs::write(&from, "ricevuto").unwrap();
        std::fs::write(dir.path().join("c.txt"), "mio").unwrap();
        let error = rename_checking(&from, &dir.path().join("c.txt"), |_| false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(read(&from), "ricevuto");
        assert_eq!(read(&dir.path().join("c.txt")), "mio");
    }

    #[test]
    fn a_file_deeper_than_max_path_is_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let deep = (0..30).fold(dir.path().to_path_buf(), |path, _| path.join("cartella"));
        assert!(deep.as_os_str().len() > 260);
        std::fs::create_dir_all(&deep).unwrap();
        let from = deep.join("d.txt.part");
        std::fs::write(&from, "lontano").unwrap();
        let placed = rename_to_free_name(&from, &deep.join("d.txt")).unwrap();
        assert_eq!(read(&placed), "lontano");
    }

    #[test]
    fn long_paths_are_verbatim() {
        let long = |path: &str| long_path(Path::new(path)).unwrap().to_string();
        assert_eq!(long(r"C:\x\..\y\a.txt"), r"\\?\C:\y\a.txt");
        assert_eq!(long(r"\\server\share\a.txt"), r"\\?\UNC\server\share\a.txt");
        assert_eq!(long(r"\\?\C:\y\a.txt"), r"\\?\C:\y\a.txt");
    }
}
