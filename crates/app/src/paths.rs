//! Where wshare keeps its per-user files.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// `%APPDATA%\wshare`, created on demand. Holds the host's TLS identity, so
/// it must be per-user (not next to the executable).
pub fn config_dir() -> Result<PathBuf> {
    let base = std::env::var_os("APPDATA").context("APPDATA is not set")?;
    let dir = PathBuf::from(base).join("wshare");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}
