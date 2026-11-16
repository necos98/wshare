//! What the desktop interface needs of unattended access: whether the
//! service is installed, and running `wshare service ...` as an
//! administrator (Windows asks the user to confirm, UAC).

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{ERROR_CANCELLED, HANDLE};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HSTRING, PCWSTR, w};

use super::scm;

/// The unattended-access service as Windows knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    NotInstalled,
    /// Installed but not running (stopping, failed...).
    Stopped,
    Running,
}

impl ServiceState {
    pub fn installed(self) -> bool {
        self != Self::NotInstalled
    }
}

/// Reads the state of the service; needs no administrator.
pub fn installed_state() -> Result<ServiceState> {
    scm::state()
}

/// How an elevated command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevatedOutcome {
    Succeeded,
    /// The user said no to Windows' confirmation.
    Cancelled,
    /// It ran and failed with this exit code (its reason is in its own
    /// output, which an elevated process cannot hand back).
    Failed(u32),
}

/// Runs this wshare.exe with `args` as an administrator, hidden, and waits
/// for it. Blocks: never on the UI thread.
pub fn run_elevated(args: &[String]) -> Result<ElevatedOutcome> {
    let exe = std::env::current_exe().context("locating wshare.exe")?;
    let file = HSTRING::from(exe.as_os_str());
    let parameters = HSTRING::from(command_line(args));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: every string outlives the call; the process handle it
    // returns is owned right after.
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => {}
        Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => {
            return Ok(ElevatedOutcome::Cancelled);
        }
        Err(e) => return Err(e).context("asking Windows for administrator rights"),
    }
    if info.hProcess.is_invalid() {
        anyhow::bail!("Windows did not start wshare as an administrator");
    }
    // SAFETY: returned by ShellExecuteExW for us to close.
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) };
    let process = HANDLE(process.as_raw_handle());
    let mut code = 0u32;
    // SAFETY: our process handle and a valid out-parameter.
    unsafe {
        WaitForSingleObject(process, INFINITE);
        GetExitCodeProcess(process, &mut code)
    }
    .context("reading how wshare ended")?;
    Ok(match code {
        0 => ElevatedOutcome::Succeeded,
        code => ElevatedOutcome::Failed(code),
    })
}

/// `args` as one Windows command line, each quoted as the C runtime (and
/// Rust's `std::env::args`) splits them back.
fn command_line(args: &[String]) -> String {
    args.iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Backslashes before a quote are doubled, the quote escaped.
                quoted.extend(std::iter::repeat_n('\\', 2 * backslashes + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    // Backslashes before the closing quote are doubled too.
    quoted.extend(std::iter::repeat_n('\\', 2 * backslashes));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(args: &[&str]) -> String {
        command_line(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn plain_arguments_are_left_alone() {
        assert_eq!(
            line(&["service", "install", "--port", "7420"]),
            "service install --port 7420"
        );
    }

    #[test]
    fn paths_with_spaces_and_trailing_backslashes_survive() {
        assert_eq!(
            line(&["--from", r"C:\Users\Anna Rossi\AppData\Local\wshare\"]),
            r#"--from "C:\Users\Anna Rossi\AppData\Local\wshare\\""#
        );
        assert_eq!(line(&[r"C:\a b\c"]), r#""C:\a b\c""#);
    }

    #[test]
    fn quotes_and_empty_arguments_are_escaped() {
        assert_eq!(line(&[r#"say "hi""#]), r#""say \"hi\"""#);
        assert_eq!(line(&[r#"a\"b"#]), r#""a\\\"b""#);
        assert_eq!(line(&[""]), r#""""#);
    }

    #[test]
    fn only_a_missing_service_counts_as_not_installed() {
        assert!(!ServiceState::NotInstalled.installed());
        assert!(ServiceState::Stopped.installed());
        assert!(ServiceState::Running.installed());
    }
}
