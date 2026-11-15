//! Where wshare keeps its per-user files.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// The viewer's own TLS identity, apart from the host's `cert.der` /
/// `key.der`: both may be created at the same time by different processes.
const VIEWER_IDENTITY_DIR: &str = "viewer";
const AUTHORIZED_VIEWERS_FILE: &str = "authorized_viewers.toml";
const KNOWN_HOSTS_FILE: &str = "known_hosts.toml";

/// `%APPDATA%\wshare`, created on demand. Holds the host's TLS identity, so
/// it must be per-user (not next to the executable).
pub fn config_dir() -> Result<PathBuf> {
    let base = std::env::var_os("APPDATA").context("APPDATA is not set")?;
    let dir = PathBuf::from(base).join("wshare");
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
