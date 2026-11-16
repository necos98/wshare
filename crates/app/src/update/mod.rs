//! Updates from the GitHub Releases of wshare.
//!
//! Every release publishes `latest.json`, its Ed25519 signature
//! `latest.json.sig` and the executables (see the `release` crate). An
//! update:
//!
//! 1. `check`: downloads manifest and signature from
//!    [`LATEST_URL`], verifies the signature with the public key compiled
//!    in and compares the version with this one;
//! 2. `install`: downloads `wshare.exe` (and `wshare-cli.exe`, when the
//!    release has it) next to the running one as `*.new`, checks size and
//!    SHA-256 against the signed manifest, and swaps them in: the running
//!    exe is renamed to `*.old` (Windows allows renaming a running
//!    program, not overwriting it) and the new one takes its name;
//! 3. the window then restarts on the new exe (see `gui::updates`);
//!    [`clean_leftovers`] removes the `*.old` files at the next start.
//!
//! Downloads go through `curl.exe` from System32 (part of Windows 10 and
//! later), HTTPS only. Nothing runs as an administrator: a wshare in a
//! folder the user cannot write (Program Files) cannot update itself and
//! says so. The copy the unattended-access service runs is updated by its
//! «Aggiorna…» (see `unattended::install`).

mod check;
mod fetch;
mod install;

use std::path::PathBuf;

use anyhow::{Context, Result};

#[cfg(test)]
pub(crate) use check::tests::{available as test_available, manifest as test_manifest};
pub use check::{Available, check};
pub use install::{clean_leftovers, install};

/// Where the latest release's files are downloaded from.
pub const LATEST_URL: &str = "https://github.com/necos98/wshare/releases/latest/download";
/// Points [`LATEST_URL`] elsewhere (a test server, a mirror). The signature
/// is still checked with the compiled-in key, so it cannot change what
/// gets installed.
const URL_ENV: &str = "WSHARE_UPDATE_URL";

/// The program files an update replaces; the first one must be in it.
pub const PROGRAM_FILES: [&str; 2] = ["wshare.exe", "wshare-cli.exe"];

/// The version of this wshare.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Why an update could not be checked or installed (the window says it in
/// Italian, see `text::update_failure`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateFailure {
    /// A download failed (offline, GitHub unreachable, no release yet...).
    Download(String),
    /// The manifest is not signed by wshare's key.
    BadSignature,
    /// The signed manifest cannot be used (no wshare.exe in it, ...).
    BadManifest(String),
    /// A downloaded file is not the one the manifest describes.
    Corrupted(String),
    /// The program's folder cannot be written (e.g. Program Files).
    FolderNotWritable(PathBuf),
    /// Swapping the files failed; the old ones are back.
    Install(String),
}

impl std::fmt::Display for UpdateFailure {
    /// English, for the terminal.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Download(detail) => write!(f, "download failed: {detail}"),
            Self::BadSignature => write!(f, "the release is not signed by wshare's key"),
            Self::BadManifest(detail) => write!(f, "unusable release manifest: {detail}"),
            Self::Corrupted(name) => write!(f, "{name} does not match the signed manifest"),
            Self::FolderNotWritable(dir) => write!(f, "cannot write to {}", dir.display()),
            Self::Install(detail) => write!(f, "replacing the program failed: {detail}"),
        }
    }
}

impl std::error::Error for UpdateFailure {}

/// The base URL of the latest release's files.
fn latest_url() -> String {
    std::env::var(URL_ENV)
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| LATEST_URL.to_string())
}

/// The folder of the running wshare.exe.
pub fn program_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating wshare.exe")?;
    exe.parent()
        .map(PathBuf::from)
        .context("wshare.exe has no folder")
}

/// `wshare update [--check]`.
pub fn run(check_only: bool) -> Result<()> {
    println!(
        "wshare {}: checking {} ...",
        current_version(),
        latest_url()
    );
    let Some(available) = check()? else {
        println!("Up to date.");
        return Ok(());
    };
    println!("Version {} is available.", available.version());
    if check_only {
        println!("Run `wshare update` to install it.");
        return Ok(());
    }
    println!("Downloading...");
    install(&available, &program_dir()?)?;
    println!(
        "Installed wshare {}: it is used from the next start.",
        available.version()
    );
    Ok(())
}
