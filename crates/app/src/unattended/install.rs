//! `wshare service install`, `uninstall` and `status`.
//!
//! Install (as an administrator; again to update the copies):
//! 1. the password, if any, is hashed: only the hash is kept;
//! 2. the service is stopped if it runs, so its files can be replaced;
//! 3. wshare.exe is copied to `%ProgramFiles%\wshare-service`: the service
//!    runs as SYSTEM, so its program must not be a file a standard user
//!    could replace;
//! 4. the identity and the authorized devices of the user are copied to
//!    the protected folder `%ProgramData%\wshare-service`, made anew, with
//!    the settings of the service (password hash, port, monitor, frame
//!    rate, bitrate, codec, clipboard and sound);
//! 5. the service is created (or updated) and started.
//!
//! Uninstall stops and deletes the service and removes both folders.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use discovery::computer_name;
use net::Identity;
use net::identity::IDENTITY_FILES;
use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
use windows::core::{HSTRING, PCWSTR};

use super::command::{InstallOptions, PasswordChoice};
use super::config::ServiceConfig;
use super::elevation::ServiceState;
use super::password::PasswordHash;
use super::{locations, password_prompt, protected_dir, scm};
use crate::paths::{self, AUTHORIZED_VIEWERS_FILE};
use crate::trust::AuthorizedDevices;

/// How long the service gets to stop (its agent says goodbye first).
const STOP_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) fn install(options: &InstallOptions) -> Result<()> {
    scm::ensure_administrator()?;
    let password = match &options.password {
        PasswordChoice::None => None,
        PasswordChoice::Prompt => Some(PasswordHash::new(&password_prompt::ask()?)?),
        PasswordChoice::Hash(hash) => Some(hash.clone()),
    };
    let source = match &options.from {
        Some(dir) => dir.clone(),
        None => paths::config_dir()?,
    };
    // The identity viewers know this PC by: created now if the user never
    // shared, so the window and the service keep the same one.
    Identity::load_or_create(&source, &computer_name())
        .with_context(|| format!("reading the identity in {}", source.display()))?;
    let authorized = authorized_devices(&source)?;

    scm::stop(STOP_TIMEOUT)?;
    let program = locations::program_dir()?;
    std::fs::create_dir_all(&program).with_context(|| format!("creating {}", program.display()))?;
    let exe = locations::installed_exe(&program);
    copy_this_exe(&exe)?;

    let data = locations::data_dir()?;
    protected_dir::recreate(&data)?;
    for name in IDENTITY_FILES.iter().chain([&AUTHORIZED_VIEWERS_FILE]) {
        copy_if_present(&source.join(name), &data.join(name))?;
    }
    ServiceConfig {
        settings: options.settings.clone(),
        password,
    }
    .save(&locations::config_file(&data))?;

    scm::install(&exe)?;
    scm::start()?;
    println!(
        "Unattended access is on (UDP port {}).",
        options.settings.port
    );
    if authorized == 0 {
        println!(
            "No device is authorized yet: unattended access admits only devices that \
             connected once with the PIN. Connect them, then run this command again."
        );
    } else {
        println!("{authorized} authorized device(s) can connect.");
    }
    Ok(())
}

/// How many devices the user authorized; fails on a damaged list rather
/// than install a service nobody may use without knowing why.
fn authorized_devices(source: &Path) -> Result<usize> {
    let (devices, warning) = AuthorizedDevices::load(source.join(AUTHORIZED_VIEWERS_FILE));
    if let Some(warning) = warning {
        anyhow::bail!("the authorized devices cannot be read: {warning}");
    }
    Ok(devices.list().len())
}

fn copy_this_exe(target: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("locating wshare.exe")?;
    let same = std::fs::canonicalize(&exe).ok() == std::fs::canonicalize(target).ok();
    if !same {
        std::fs::copy(&exe, target)
            .with_context(|| format!("copying wshare.exe to {}", target.display()))?;
    }
    Ok(())
}

fn copy_if_present(from: &Path, to: &Path) -> Result<()> {
    match std::fs::copy(from, to) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("copying {}", from.display())),
    }
}

pub(super) fn uninstall() -> Result<()> {
    scm::ensure_administrator()?;
    let removed = scm::remove(STOP_TIMEOUT)?;
    remove_dir(&locations::data_dir()?)?;
    remove_dir(&locations::program_dir()?)?;
    if removed {
        println!("Unattended access is off: the service and its files are gone.");
    } else {
        println!("Unattended access was not installed.");
    }
    Ok(())
}

/// Removes `dir`. A program still running from it (e.g. this command, run
/// from the installed copy) is removed at the next restart of Windows.
fn remove_dir(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => remove_at_restart(dir),
    }
}

fn remove_at_restart(dir: &Path) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            remove_at_restart(&path)?;
        } else if path.exists() {
            delete_at_restart(&path)?;
        }
    }
    delete_at_restart(dir)?;
    println!(
        "{} is in use: it will be removed when Windows restarts.",
        dir.display()
    );
    Ok(())
}

fn delete_at_restart(path: &Path) -> Result<()> {
    // SAFETY: the path outlives the call; no new name means "delete".
    unsafe {
        MoveFileExW(
            &HSTRING::from(path.as_os_str()),
            PCWSTR::null(),
            MOVEFILE_DELAY_UNTIL_REBOOT,
        )
    }
    .with_context(|| format!("removing {} at restart", path.display()))
}

pub(super) fn print_status() -> Result<()> {
    let text = match scm::state()? {
        ServiceState::NotInstalled => "not installed",
        ServiceState::Stopped => "installed, not running",
        ServiceState::Running => "installed and running",
    };
    println!("Unattended access: {text}.");
    Ok(())
}
