//! `wshare-cli.exe`: a console-subsystem front for `wshare.exe`.
//!
//! wshare.exe is a "windows" subsystem program (no console window when
//! double-clicked), so terminals do not wait for it: its output lands after
//! the prompt, `view`'s PIN prompt competes with the shell for input, and
//! the exit code is lost. This shim is an ordinary console program that
//! runs the `wshare.exe` next to it with the same arguments and the same
//! standard handles, waits, and exits with its exit code.
//!
//! - No arguments run `wshare.exe help` (usage), not the interface: whoever
//!   types `wshare-cli` in a terminal wants the command line; the GUI is
//!   one double-click on wshare.exe away.
//! - Ctrl+C / Ctrl+Break are ignored here. wshare.exe attaches to this
//!   console, so it receives them too and stops cleanly (keys released,
//!   Bye sent); the shim then returns its exit code.

mod ctrl;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// File name of the real program, expected next to the shim.
const TARGET_EXE: &str = "wshare.exe";
/// Exit code when the shim itself fails (same as wshare's errors).
const FAILURE: u8 = 1;

fn main() -> ExitCode {
    match run() {
        Ok(code) => match exit_code(code) {
            Exit::Byte(code) => ExitCode::from(code),
            Exit::Full(code) => std::process::exit(code),
        },
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::from(FAILURE)
        }
    }
}

/// Runs wshare.exe to completion; returns its exit code (`None` if it has
/// none, which does not happen on Windows).
fn run() -> Result<Option<i32>, String> {
    let shim =
        std::env::current_exe().map_err(|e| format!("cannot locate wshare-cli.exe itself: {e}"))?;
    let target = target_path(&shim);
    if !target.is_file() {
        return Err(missing_message(&target));
    }
    // Before spawning, so no Ctrl+C can end us while the child runs.
    ctrl::ignore_interrupts();
    let status = Command::new(&target)
        .args(child_args(std::env::args_os().skip(1)))
        .status()
        .map_err(|e| format!("cannot start {}: {e}", target.display()))?;
    Ok(status.code())
}

/// `wshare.exe` in the shim's own directory.
fn target_path(shim: &Path) -> PathBuf {
    shim.with_file_name(TARGET_EXE)
}

fn missing_message(target: &Path) -> String {
    format!(
        "{TARGET_EXE} not found next to wshare-cli.exe (expected {}); keep both files in the same folder",
        target.display()
    )
}

/// The child's arguments: ours unchanged, or `help` when there are none.
fn child_args(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let args: Vec<OsString> = args.into_iter().collect();
    if args.is_empty() {
        vec!["help".into()]
    } else {
        args
    }
}

/// How to exit with the child's code.
#[derive(Debug, PartialEq, Eq)]
enum Exit {
    /// Fits [`ExitCode`] (returned from `main`, so destructors run).
    Byte(u8),
    /// A full 32-bit Windows code (e.g. 0xC000013A after an unhandled
    /// Ctrl+C), passed to `std::process::exit` unchanged.
    Full(i32),
}

fn exit_code(child: Option<i32>) -> Exit {
    match child {
        None => Exit::Byte(FAILURE),
        Some(code) => u8::try_from(code).map_or(Exit::Full(code), Exit::Byte),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn no_arguments_show_the_usage() {
        assert_eq!(child_args(Vec::new()), os(&["help"]));
    }

    #[test]
    fn arguments_pass_through_unchanged() {
        let args = os(&["view", "192.168.1.20", "--pin", "123 456"]);
        assert_eq!(child_args(args.clone()), args);
    }

    #[test]
    fn target_is_next_to_the_shim() {
        let shim = Path::new(r"C:\Tools\wshare\wshare-cli.exe");
        assert_eq!(target_path(shim), Path::new(r"C:\Tools\wshare\wshare.exe"));
    }

    #[test]
    fn missing_target_message_names_the_path() {
        let message = missing_message(Path::new(r"C:\x\wshare.exe"));
        assert!(message.contains(r"C:\x\wshare.exe"), "{message}");
        assert!(message.starts_with("wshare.exe not found"), "{message}");
    }

    #[test]
    fn small_exit_codes_propagate() {
        assert_eq!(exit_code(Some(0)), Exit::Byte(0));
        assert_eq!(exit_code(Some(1)), Exit::Byte(1));
        assert_eq!(exit_code(None), Exit::Byte(FAILURE));
    }

    #[test]
    fn large_windows_exit_codes_propagate_unchanged() {
        let ctrl_c_exit = 0xC000_013A_u32 as i32;
        assert_eq!(exit_code(Some(ctrl_c_exit)), Exit::Full(ctrl_c_exit));
        assert_eq!(exit_code(Some(256)), Exit::Full(256));
    }
}
