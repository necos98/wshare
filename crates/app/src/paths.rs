//! Where wshare keeps its per-user files.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};

use crate::data_migration;

/// The viewer's own TLS identity, apart from the host's `cert.der` /
/// `key.der`: both may be created at the same time by different processes.
const VIEWER_IDENTITY_DIR: &str = "viewer";
const AUTHORIZED_VIEWERS_FILE: &str = "authorized_viewers.toml";
const KNOWN_HOSTS_FILE: &str = "known_hosts.toml";
const LOGS_DIR: &str = "logs";
const APP_DIR: &str = "wshare";

/// Resolved once: the migration must run (and warn) only once per process.
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// `%LOCALAPPDATA%\wshare`, created on demand. Holds the TLS private keys,
/// so it must be local to this PC: the roaming profile (`%APPDATA%`) would
/// copy them to every PC the user signs in to, all with the same identity.
///
/// Files left in the old `%APPDATA%\wshare` are moved here the first time
/// (see [`resolve_data_dir`]).
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = DATA_DIR.get() {
        return Ok(dir.clone());
    }
    let local = env_dir("LOCALAPPDATA")?;
    let legacy = env_dir("APPDATA").ok().map(|base| base.join(APP_DIR));
    let (dir, warnings) = resolve_data_dir(&local.join(APP_DIR), legacy.as_deref())?;
    for warning in warnings {
        eprintln!("wshare: {warning}");
    }
    Ok(DATA_DIR.get_or_init(|| dir).clone())
}

/// Where the opt-in diagnostics are written.
pub fn logs_dir() -> Result<PathBuf> {
    let dir = config_dir()?.join(LOGS_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// Folder of the certificate a viewer presents to hosts.
pub fn viewer_identity_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join(VIEWER_IDENTITY_DIR))
}

/// Host side: viewers admitted without a PIN (see `trust::AuthorizedDevices`).
pub fn authorized_viewers_file() -> Result<PathBuf> {
    Ok(config_dir()?.join(AUTHORIZED_VIEWERS_FILE))
}

/// Viewer side: hosts pinned by fingerprint (see `trust::KnownHosts`).
pub fn known_hosts_file() -> Result<PathBuf> {
    Ok(config_dir()?.join(KNOWN_HOSTS_FILE))
}

fn env_dir(name: &str) -> Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .with_context(|| format!("{name} is not set"))
}

/// The folder to use given the new location `current` and the `legacy` one
/// (if any), plus warnings to show. Pure with respect to the environment.
///
/// When `legacy` exists its files are moved into `current` (never replacing
/// what is already there). If that fails the legacy folder is used as it
/// is: a new identity must never be generated just because a move failed,
/// since that would silently change the fingerprint viewers have pinned.
fn resolve_data_dir(current: &Path, legacy: Option<&Path>) -> Result<(PathBuf, Vec<String>)> {
    let mut warnings = Vec::new();
    let legacy = legacy.filter(|legacy| *legacy != current && legacy.is_dir());
    let Some(legacy) = legacy else {
        std::fs::create_dir_all(current)
            .with_context(|| format!("creating {}", current.display()))?;
        return Ok((current.to_path_buf(), warnings));
    };
    match data_migration::migrate(legacy, current) {
        Ok(migration) => {
            warnings.extend(migration.leftovers.iter().map(|path| {
                format!(
                    "moved to {} but could not remove the old copy {}",
                    current.display(),
                    path.display()
                )
            }));
            Ok((current.to_path_buf(), warnings))
        }
        Err(error) => {
            warnings.push(format!(
                "could not move {} to {} ({error:#}); keeping the old folder",
                legacy.display(),
                current.display()
            ));
            Ok((legacy.to_path_buf(), warnings))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn creates_the_folder_when_there_is_nothing_to_migrate() {
        let tmp = tempfile::tempdir().unwrap();
        let current = tmp.path().join("local").join("wshare");

        let (dir, warnings) = resolve_data_dir(&current, None).unwrap();

        assert_eq!(dir, current);
        assert!(current.is_dir());
        assert!(warnings.is_empty());
    }

    #[test]
    fn migrates_identity_and_stores_byte_for_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("roaming").join("wshare");
        let current = tmp.path().join("local").join("wshare");
        fs::create_dir_all(legacy.join("viewer")).unwrap();
        let cert: Vec<u8> = (0..=255).rev().collect();
        fs::write(legacy.join("cert.der"), &cert).unwrap();
        fs::write(legacy.join("key.der"), b"private key").unwrap();
        fs::write(legacy.join("viewer").join("cert.der"), b"viewer cert").unwrap();
        fs::write(legacy.join(AUTHORIZED_VIEWERS_FILE), b"authorized").unwrap();
        fs::write(legacy.join(KNOWN_HOSTS_FILE), b"known").unwrap();

        let (dir, warnings) = resolve_data_dir(&current, Some(&legacy)).unwrap();

        assert_eq!(dir, current);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(fs::read(current.join("cert.der")).unwrap(), cert);
        assert_eq!(fs::read(current.join("key.der")).unwrap(), b"private key");
        assert_eq!(
            fs::read(current.join("viewer").join("cert.der")).unwrap(),
            b"viewer cert"
        );
        assert_eq!(fs::read(current.join(KNOWN_HOSTS_FILE)).unwrap(), b"known");
        assert_eq!(
            fs::read(current.join(AUTHORIZED_VIEWERS_FILE)).unwrap(),
            b"authorized"
        );
    }

    #[test]
    fn a_second_run_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("roaming").join("wshare");
        let current = tmp.path().join("local").join("wshare");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("cert.der"), b"first").unwrap();
        resolve_data_dir(&current, Some(&legacy)).unwrap();
        // A later roaming copy (e.g. from an old install on another PC)
        // must not replace the identity already in use here.
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("cert.der"), b"stale").unwrap();

        let (dir, warnings) = resolve_data_dir(&current, Some(&legacy)).unwrap();

        assert_eq!(dir, current);
        assert!(warnings.is_empty());
        assert_eq!(fs::read(current.join("cert.der")).unwrap(), b"first");
    }

    #[test]
    fn a_failed_migration_warns_and_keeps_using_the_old_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("roaming").join("wshare");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("cert.der"), b"certificate").unwrap();
        let blocker = tmp.path().join("local");
        fs::write(&blocker, b"a file where the folder should go").unwrap();

        let (dir, warnings) = resolve_data_dir(&blocker.join("wshare"), Some(&legacy)).unwrap();

        assert_eq!(dir, legacy, "no new identity may be started");
        assert_eq!(warnings.len(), 1);
        assert_eq!(fs::read(legacy.join("cert.der")).unwrap(), b"certificate");
    }

    #[test]
    fn same_folder_twice_is_not_a_migration() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("wshare");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cert.der"), b"certificate").unwrap();

        let (resolved, warnings) = resolve_data_dir(&dir, Some(&dir)).unwrap();

        assert_eq!(resolved, dir);
        assert!(warnings.is_empty());
        assert_eq!(fs::read(dir.join("cert.der")).unwrap(), b"certificate");
    }
}
