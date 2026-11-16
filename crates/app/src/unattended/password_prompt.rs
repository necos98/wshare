//! Asking for the password of unattended access on the terminal, without
//! showing what is typed, twice to rule out a typo.

use std::io::{BufRead, Write};

use anyhow::{Context, Result, bail};
use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
};

/// The password typed twice the same; fails if the two differ or nothing
/// was typed.
pub(super) fn ask() -> Result<String> {
    let first = ask_once("Password for unattended access: ")?;
    if first.trim().is_empty() {
        bail!("no password typed");
    }
    let second = ask_once("Type it again: ")?;
    if first != second {
        bail!("the two passwords differ");
    }
    Ok(first)
}

fn ask_once(question: &str) -> Result<String> {
    print!("{question}");
    std::io::stdout().flush()?;
    let line = {
        let _hidden = HiddenInput::start();
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .context("reading the password")?;
        line
    };
    // The Enter key was not echoed either.
    println!();
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Keeps the console from echoing what is typed while it exists; does
/// nothing when the input is not a console (e.g. a pipe).
struct HiddenInput(Option<CONSOLE_MODE>);

impl HiddenInput {
    fn start() -> Self {
        // SAFETY: plain console calls on the standard input handle; the
        // mode is restored by `Drop`.
        unsafe {
            let Ok(input) = GetStdHandle(STD_INPUT_HANDLE) else {
                return Self(None);
            };
            let mut mode = CONSOLE_MODE::default();
            if GetConsoleMode(input, &mut mode).is_err()
                || SetConsoleMode(input, mode & !ENABLE_ECHO_INPUT).is_err()
            {
                return Self(None);
            }
            Self(Some(mode))
        }
    }
}

impl Drop for HiddenInput {
    fn drop(&mut self) {
        if let Some(mode) = self.0
            // SAFETY: as in `start`.
            && let Ok(input) = unsafe { GetStdHandle(STD_INPUT_HANDLE) }
        {
            // SAFETY: restores the mode read in `start`.
            let _ = unsafe { SetConsoleMode(input, mode) };
        }
    }
}
