//! Puts a verified release in place of the running program.
//!
//! Every file is downloaded beside its target as `<name>.new` and checked
//! against the signed manifest before anything is touched; only then the
//! files are swapped: `<name>` → `<name>.old`, `<name>.new` → `<name>`. A
//! swap that fails half-way puts back what it had moved.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use release::{FileEntry, sha256_hex};

use super::{Available, PROGRAM_FILES, UpdateFailure, fetch};

const NEW_SUFFIX: &str = ".new";
const OLD_SUFFIX: &str = ".old";
/// Older copies still in use (a viewer window started before the last
/// update) keep their name; the next free one is taken.
const MAX_OLD_COPIES: usize = 16;

/// Downloads `available` and puts it in place of the wshare in `dir`.
pub fn install(available: &Available, dir: &Path) -> Result<(), UpdateFailure> {
    install_with(available, dir, fetch::to_file)
}

/// [`install`], downloading with `download(url, target)`.
fn install_with(
    available: &Available,
    dir: &Path,
    download: impl Fn(&str, &Path) -> Result<(), UpdateFailure>,
) -> Result<(), UpdateFailure> {
    let entries: Vec<&FileEntry> = PROGRAM_FILES
        .iter()
        .filter_map(|name| available.manifest().file(name))
        .collect();
    let mut staged = Vec::new();
    let result = (|| {
        for entry in &entries {
            let target = dir.join(&entry.name);
            let new = with_suffix(&target, NEW_SUFFIX);
            claim(&new, dir)?;
            staged.push(new.clone());
            download(&entry.url, &new)?;
            verify(&new, entry)?;
        }
        swap(dir, &entries)
    })();
    for new in staged {
        let _ = fs::remove_file(new);
    }
    result
}

/// Creates `path` empty: the first write in the folder, so a folder the
/// user cannot write is told apart from a failed download.
fn claim(path: &Path, dir: &Path) -> Result<(), UpdateFailure> {
    match fs::File::create(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            Err(UpdateFailure::FolderNotWritable(dir.to_path_buf()))
        }
        Err(e) => Err(UpdateFailure::Install(format!("{}: {e}", path.display()))),
    }
}

fn verify(path: &Path, entry: &FileEntry) -> Result<(), UpdateFailure> {
    let bytes =
        fs::read(path).map_err(|e| UpdateFailure::Install(format!("{}: {e}", path.display())))?;
    if bytes.len() as u64 != entry.size || !sha256_hex(&bytes).eq_ignore_ascii_case(&entry.sha256) {
        return Err(UpdateFailure::Corrupted(entry.name.clone()));
    }
    Ok(())
}

/// One finished step of the swap, to undo.
enum Step {
    /// `current` was renamed to `old`.
    MovedAside { current: PathBuf, old: PathBuf },
    /// `new` became `current`.
    Placed { new: PathBuf, current: PathBuf },
}

fn swap(dir: &Path, entries: &[&FileEntry]) -> Result<(), UpdateFailure> {
    let mut done = Vec::new();
    let result = (|| {
        for entry in entries {
            let current = dir.join(&entry.name);
            let new = with_suffix(&current, NEW_SUFFIX);
            if current.exists() {
                let old = free_old_name(&current)?;
                fs::rename(&current, &old).map_err(|e| install_error(&current, &e))?;
                done.push(Step::MovedAside {
                    current: current.clone(),
                    old,
                });
            }
            fs::rename(&new, &current).map_err(|e| install_error(&new, &e))?;
            done.push(Step::Placed { new, current });
        }
        Ok(())
    })();
    if result.is_err() {
        for step in done.into_iter().rev() {
            let _ = match step {
                Step::Placed { new, current } => fs::rename(current, new),
                Step::MovedAside { current, old } => fs::rename(old, current),
            };
        }
    }
    result
}

/// `<name>.old`, or `<name>.<n>.old` when that one is still in use.
fn free_old_name(current: &Path) -> Result<PathBuf, UpdateFailure> {
    let plain = with_suffix(current, OLD_SUFFIX);
    if remove_if_present(&plain) {
        return Ok(plain);
    }
    (1..=MAX_OLD_COPIES)
        .map(|n| with_suffix(current, &format!(".{n}{OLD_SUFFIX}")))
        .find(|candidate| remove_if_present(candidate))
        .ok_or_else(|| {
            UpdateFailure::Install(format!("too many old copies of {}", current.display()))
        })
}

/// Whether `path` is free now (absent, or just removed).
fn remove_if_present(path: &Path) -> bool {
    match fs::remove_file(path) {
        Ok(()) => true,
        Err(e) => e.kind() == io::ErrorKind::NotFound,
    }
}

/// Removes what earlier updates left in `dir`: the replaced program files
/// (no longer running) and downloads that did not finish. Files still in
/// use stay until a later start.
pub fn clean_leftovers(dir: &Path) {
    let Ok(listing) = fs::read_dir(dir) else {
        return;
    };
    for entry in listing.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if is_leftover(&name) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn is_leftover(name: &str) -> bool {
    PROGRAM_FILES.iter().any(|program| {
        name.strip_prefix(program).is_some_and(|rest| {
            rest == NEW_SUFFIX
                || rest.strip_suffix(OLD_SUFFIX).is_some_and(|middle| {
                    middle.is_empty()
                        || middle
                            .strip_prefix('.')
                            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                })
        })
    })
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn install_error(path: &Path, e: &io::Error) -> UpdateFailure {
    UpdateFailure::Install(format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::check::tests::{available, manifest};
    use super::*;

    const NEW_EXE: &[u8] = b"new wshare";
    const NEW_CLI: &[u8] = b"new cli";

    /// A download that serves `files` by the name at the end of the URL.
    fn server(files: &[(&str, &[u8])]) -> impl Fn(&str, &Path) -> Result<(), UpdateFailure> {
        let files: HashMap<String, Vec<u8>> = files
            .iter()
            .map(|(n, b)| ((*n).to_string(), b.to_vec()))
            .collect();
        move |url, target| {
            let name = url.rsplit('/').next().unwrap();
            let bytes = files
                .get(name)
                .ok_or_else(|| UpdateFailure::Download(format!("{url}: 404")))?;
            fs::write(target, bytes).unwrap();
            Ok(())
        }
    }

    fn folder_with_old_program() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("wshare.exe"), b"old wshare").unwrap();
        fs::write(dir.path().join("wshare-cli.exe"), b"old cli").unwrap();
        dir
    }

    fn read(dir: &Path, name: &str) -> Vec<u8> {
        fs::read(dir.join(name)).unwrap()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn replaces_both_programs_and_keeps_the_old_ones_aside() {
        let dir = folder_with_old_program();
        let files = [("wshare.exe", NEW_EXE), ("wshare-cli.exe", NEW_CLI)];
        let update = available(manifest("0.2.0", &files));
        install_with(&update, dir.path(), server(&files)).unwrap();
        assert_eq!(read(dir.path(), "wshare.exe"), NEW_EXE);
        assert_eq!(read(dir.path(), "wshare-cli.exe"), NEW_CLI);
        assert_eq!(read(dir.path(), "wshare.exe.old"), b"old wshare");
        clean_leftovers(dir.path());
        assert_eq!(names(dir.path()), ["wshare-cli.exe", "wshare.exe"]);
    }

    #[test]
    fn a_corrupted_download_changes_nothing() {
        let dir = folder_with_old_program();
        let update = available(manifest(
            "0.2.0",
            &[("wshare.exe", NEW_EXE), ("wshare-cli.exe", NEW_CLI)],
        ));
        let served = [
            ("wshare.exe", NEW_EXE),
            ("wshare-cli.exe", b"tampered".as_slice()),
        ];
        assert_eq!(
            install_with(&update, dir.path(), server(&served)),
            Err(UpdateFailure::Corrupted("wshare-cli.exe".into()))
        );
        assert_eq!(read(dir.path(), "wshare.exe"), b"old wshare");
        assert_eq!(read(dir.path(), "wshare-cli.exe"), b"old cli");
        assert_eq!(names(dir.path()), ["wshare-cli.exe", "wshare.exe"]);
    }

    #[test]
    fn a_failed_download_changes_nothing() {
        let dir = folder_with_old_program();
        let update = available(manifest("0.2.0", &[("wshare.exe", NEW_EXE)]));
        let result = install_with(&update, dir.path(), server(&[]));
        assert!(matches!(result, Err(UpdateFailure::Download(_))));
        assert_eq!(names(dir.path()), ["wshare-cli.exe", "wshare.exe"]);
    }

    #[test]
    fn a_release_without_the_cli_leaves_it_alone() {
        let dir = folder_with_old_program();
        let files = [("wshare.exe", NEW_EXE)];
        install_with(
            &available(manifest("0.2.0", &files)),
            dir.path(),
            server(&files),
        )
        .unwrap();
        assert_eq!(read(dir.path(), "wshare.exe"), NEW_EXE);
        assert_eq!(read(dir.path(), "wshare-cli.exe"), b"old cli");
    }

    #[test]
    fn a_swap_that_fails_half_way_is_undone() {
        let dir = folder_with_old_program();
        let update = available(manifest(
            "0.2.0",
            &[("wshare.exe", NEW_EXE), ("wshare-cli.exe", NEW_CLI)],
        ));
        let entries: Vec<&FileEntry> = update.manifest().files.iter().collect();
        // wshare.exe is swapped in, then the CLI's download is missing.
        fs::write(dir.path().join("wshare.exe.new"), NEW_EXE).unwrap();
        assert!(matches!(
            swap(dir.path(), &entries),
            Err(UpdateFailure::Install(_))
        ));
        assert_eq!(read(dir.path(), "wshare.exe"), b"old wshare");
        assert_eq!(read(dir.path(), "wshare-cli.exe"), b"old cli");
        assert_eq!(read(dir.path(), "wshare.exe.new"), NEW_EXE);
    }

    #[test]
    fn an_old_copy_still_in_use_gets_a_numbered_name() {
        let dir = folder_with_old_program();
        // A directory cannot be removed as a file: it stands for a copy in use.
        fs::create_dir(dir.path().join("wshare.exe.old")).unwrap();
        let files = [("wshare.exe", NEW_EXE)];
        install_with(
            &available(manifest("0.2.0", &files)),
            dir.path(),
            server(&files),
        )
        .unwrap();
        assert_eq!(read(dir.path(), "wshare.exe.1.old"), b"old wshare");
    }

    #[test]
    fn only_update_leftovers_are_cleaned() {
        for name in [
            "wshare.exe.old",
            "wshare.exe.3.old",
            "wshare-cli.exe.new",
            "WSHARE.EXE.OLD",
        ] {
            assert!(is_leftover(&name.to_ascii_lowercase()), "{name}");
        }
        for name in [
            "wshare.exe",
            "wshare.exe.x.old",
            "settings.toml",
            "other.exe.old",
            "wshare.exe..old",
        ] {
            assert!(!is_leftover(name), "{name}");
        }
    }
}
