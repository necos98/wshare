//! Where unattended access keeps its files. Both folders are writable by
//! administrators only, since what is in them runs or decides as SYSTEM.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use windows::Win32::UI::Shell::{FOLDERID_ProgramData, FOLDERID_ProgramFiles};

use crate::paths;

/// `%ProgramFiles%\wshare`: the copy of wshare.exe the service runs.
const PROGRAM_DIR: &str = "wshare";
const EXE_NAME: &str = "wshare.exe";
/// `%ProgramData%\wshare-service`: identity, authorized devices, settings
/// and log of the service (see `protected_dir`).
const DATA_DIR: &str = "wshare-service";
const CONFIG_FILE: &str = "service.toml";
const LOG_FILE: &str = "service.log";

pub(super) fn program_dir() -> Result<PathBuf> {
    Ok(paths::known_folder(&FOLDERID_ProgramFiles)
        .context("locating Program Files")?
        .join(PROGRAM_DIR))
}

/// The wshare.exe the service and its agent run.
pub(super) fn installed_exe(program_dir: &Path) -> PathBuf {
    program_dir.join(EXE_NAME)
}

pub(super) fn data_dir() -> Result<PathBuf> {
    Ok(paths::known_folder(&FOLDERID_ProgramData)
        .context("locating ProgramData")?
        .join(DATA_DIR))
}

pub(super) fn config_file(data_dir: &Path) -> PathBuf {
    data_dir.join(CONFIG_FILE)
}

pub(super) fn log_file(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_FILE)
}
