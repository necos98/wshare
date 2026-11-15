//! One-time move of wshare's files from the old roaming folder
//! (`%APPDATA%\wshare`) to the per-user local one (`%LOCALAPPDATA%\wshare`).
//!
//! Identity and trust files must arrive byte for byte: the fingerprint the
//! host announces and the devices it authorized depend on them. So files are
//! copied first and the originals removed only once everything arrived; if
//! anything fails, what was copied is undone and the old folder is left
//! untouched (the caller keeps using it rather than generating a new
//! identity).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// What a migration left behind that the user may want to know about.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Migration {
    /// Entries copied to the new folder.
    pub moved: Vec<String>,
    /// Originals that could not be removed after a successful copy.
    pub leftovers: Vec<PathBuf>,
}

/// Moves every entry of `legacy` that `current` does not have yet into
/// `current` (created if missing). Entries already in `current` win and the
/// corresponding originals stay where they are: nothing is ever overwritten.
///
/// On error the entries copied so far are removed again.
pub fn migrate(legacy: &Path, current: &Path) -> Result<Migration> {
    fs::create_dir_all(current).with_context(|| format!("creating {}", current.display()))?;
    let mut pending = Vec::new();
    for entry in fs::read_dir(legacy).with_context(|| format!("reading {}", legacy.display()))? {
        let name = entry
            .with_context(|| format!("reading {}", legacy.display()))?
            .file_name();
        if !current.join(&name).exists() {
            pending.push(name);
        }
    }

    let mut copied = Vec::new();
    for name in &pending {
        let target = current.join(name);
        copied.push(target.clone());
        if let Err(error) = copy_entry(&legacy.join(name), &target) {
            remove_all(&copied);
            return Err(error.context(format!("copying {}", legacy.join(name).display())));
        }
    }

    let mut migration = Migration::default();
    for name in pending {
        let original = legacy.join(&name);
        match remove_entry(&original) {
            Ok(()) => {}
            Err(_) => migration.leftovers.push(original),
        }
        migration.moved.push(name.to_string_lossy().into_owned());
    }
    // The legacy folder itself goes only when it is empty now.
    let _ = fs::remove_dir(legacy);
    Ok(migration)
}

fn copy_entry(from: &Path, to: &Path) -> Result<()> {
    if fs::metadata(from)?.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let name = entry?.file_name();
            copy_entry(&from.join(&name), &to.join(&name))?;
        }
    } else {
        fs::copy(from, to)?;
    }
    Ok(())
}

fn remove_entry(path: &Path) -> io::Result<()> {
    if fs::metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Best effort: this runs while undoing a failed migration, where the
/// original error is the one worth reporting.
fn remove_all(paths: &[PathBuf]) {
    for path in paths {
        let _ = remove_entry(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn files_and_folders_arrive_identical_and_originals_go() {
        let tmp = tempfile::tempdir().unwrap();
        let (legacy, current) = (tmp.path().join("roaming"), tmp.path().join("local"));
        let key: Vec<u8> = (0..=255).collect();
        write(&legacy.join("cert.der"), b"certificate");
        write(&legacy.join("key.der"), &key);
        write(&legacy.join("viewer").join("key.der"), b"viewer key");
        write(&legacy.join("known_hosts.toml"), b"[[hosts]]\n");

        let migration = migrate(&legacy, &current).unwrap();

        assert_eq!(fs::read(current.join("cert.der")).unwrap(), b"certificate");
        assert_eq!(fs::read(current.join("key.der")).unwrap(), key);
        assert_eq!(
            fs::read(current.join("viewer").join("key.der")).unwrap(),
            b"viewer key"
        );
        assert_eq!(
            fs::read(current.join("known_hosts.toml")).unwrap(),
            b"[[hosts]]\n"
        );
        assert_eq!(migration.moved.len(), 4);
        assert!(migration.leftovers.is_empty());
        assert!(!legacy.exists(), "emptied legacy folder is removed");
    }

    #[test]
    fn existing_entries_are_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let (legacy, current) = (tmp.path().join("roaming"), tmp.path().join("local"));
        write(&legacy.join("cert.der"), b"old");
        write(&legacy.join("key.der"), b"old key");
        write(&current.join("cert.der"), b"new");

        let migration = migrate(&legacy, &current).unwrap();

        assert_eq!(fs::read(current.join("cert.der")).unwrap(), b"new");
        assert_eq!(fs::read(current.join("key.der")).unwrap(), b"old key");
        assert_eq!(migration.moved, ["key.der"]);
        assert_eq!(fs::read(legacy.join("cert.der")).unwrap(), b"old");
    }

    #[test]
    fn failure_undoes_the_copy_and_keeps_the_legacy_folder() {
        use std::os::windows::fs::OpenOptionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let (legacy, current) = (tmp.path().join("roaming"), tmp.path().join("local"));
        write(&legacy.join("aa.der"), b"certificate");
        write(&legacy.join("zz.der"), b"key");
        // Opened without sharing: copying it fails after `aa.der` was copied
        // (entries are visited in name order on NTFS).
        let _lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(legacy.join("zz.der"))
            .unwrap();

        assert!(migrate(&legacy, &current).is_err());

        assert!(!current.join("aa.der").exists(), "partial copy undone");
        assert!(!current.join("zz.der").exists());
        assert_eq!(fs::read(legacy.join("aa.der")).unwrap(), b"certificate");
    }

    #[test]
    fn unusable_destination_is_an_error_and_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("roaming");
        write(&legacy.join("cert.der"), b"certificate");
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, b"a file, not a folder").unwrap();

        assert!(migrate(&legacy, &blocker.join("local")).is_err());
        assert_eq!(fs::read(legacy.join("cert.der")).unwrap(), b"certificate");
    }
}
