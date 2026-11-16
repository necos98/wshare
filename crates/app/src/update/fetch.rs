//! Downloads with the `curl.exe` of Windows (System32, not the PATH: a
//! program dropped in the current folder must not stand in for it).

use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::Win32::System::Threading::CREATE_NO_WINDOW;

use super::UpdateFailure;

/// Manifest and signature are small: they must arrive quickly.
const SMALL_TIMEOUT_SECS: &str = "30";
/// The executables: a slow line still gets them.
const LARGE_TIMEOUT_SECS: &str = "900";
const CONNECT_TIMEOUT_SECS: &str = "15";

fn curl() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    Path::new(&root).join("System32").join("curl.exe")
}

/// The bytes at `url` (a small file).
pub fn bytes(url: &str) -> Result<Vec<u8>, UpdateFailure> {
    let output = command(url, SMALL_TIMEOUT_SECS)
        .output()
        .map_err(|e| UpdateFailure::Download(format!("cannot run curl.exe: {e}")))?;
    if !output.status.success() {
        return Err(failure(url, &output.stderr));
    }
    Ok(output.stdout)
}

/// Saves `url` as `target` (overwritten).
pub fn to_file(url: &str, target: &Path) -> Result<(), UpdateFailure> {
    let output = command(url, LARGE_TIMEOUT_SECS)
        .arg("--output")
        .arg(target)
        .output()
        .map_err(|e| UpdateFailure::Download(format!("cannot run curl.exe: {e}")))?;
    if !output.status.success() {
        return Err(failure(url, &output.stderr));
    }
    Ok(())
}

fn command(url: &str, timeout: &str) -> Command {
    let mut command = Command::new(curl());
    command
        // -sS: no progress, but errors; -L: GitHub redirects downloads.
        .args([
            "-sSL",
            "--fail",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
        ])
        .args([
            "--connect-timeout",
            CONNECT_TIMEOUT_SECS,
            "--max-time",
            timeout,
        ])
        .args(["--user-agent", "wshare-updater"])
        .arg(url)
        // Run from the window: no console flashing up.
        .creation_flags(CREATE_NO_WINDOW.0);
    command
}

fn failure(url: &str, stderr: &[u8]) -> UpdateFailure {
    let detail = String::from_utf8_lossy(stderr).trim().to_string();
    UpdateFailure::Download(format!("{url}: {detail}"))
}
