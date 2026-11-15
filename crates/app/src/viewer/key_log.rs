//! Opt-in keyboard diagnostics: with `WSHARE_DEBUG_KEYS=1` every line is
//! appended to `wshare-keys.log` in the `logs` folder of the data folder
//! (`%LOCALAPPDATA%\wshare\logs`).
//!
//! The log records every key pressed in the viewer, passwords included:
//! only the exact value `1` turns it on, and it is meant for short
//! debugging sessions, after which the file should be deleted.
//!
//! Writing happens on a background thread: the keyboard hook must never
//! block (Windows skips hooks that are slow to answer), and a terminal pipe
//! can stall for a long time.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::time::Instant;

use crate::paths;

const ENABLE_VARIABLE: &str = "WSHARE_DEBUG_KEYS";
const ENABLE_VALUE: &str = "1";

static LOG: OnceLock<Option<(Sender<String>, Instant)>> = OnceLock::new();

/// Whether the value of [`ENABLE_VARIABLE`] switches the log on.
fn is_requested(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new(ENABLE_VALUE))
}

fn logger() -> Option<&'static (Sender<String>, Instant)> {
    LOG.get_or_init(|| {
        if !is_requested(std::env::var_os(ENABLE_VARIABLE).as_deref()) {
            return None;
        }
        let (path, mut file) = open_log()?;
        eprintln!("Keyboard diagnostics: {}", path.display());
        let (tx, rx) = channel::<String>();
        std::thread::Builder::new()
            .name("wshare-key-log".into())
            .spawn(move || {
                for line in rx {
                    let _ = writeln!(file, "{line}");
                    let _ = file.flush();
                }
            })
            .ok()?;
        let _ = tx.send(format!("=== wshare viewer pid {} ===", std::process::id()));
        Some((tx, Instant::now()))
    })
    .as_ref()
}

/// Opens the log, falling back to a per-process name when the usual file
/// is locked (e.g. a terminal is writing to it) — never silently disabled.
fn open_log() -> Option<(PathBuf, std::fs::File)> {
    let dir = match paths::logs_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("Keyboard diagnostics disabled: {e:#}");
            return None;
        }
    };
    let candidates = [
        dir.join("wshare-keys.log"),
        dir.join(format!("wshare-keys-{}.log", std::process::id())),
    ];
    for path in candidates {
        if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
            return Some((path, file));
        }
    }
    eprintln!(
        "Keyboard diagnostics disabled: cannot create a log in {}",
        dir.display()
    );
    None
}

pub fn enabled() -> bool {
    logger().is_some()
}

/// Queues one line, prefixed with milliseconds since logging started.
pub fn log(line: impl FnOnce() -> String) {
    if let Some((tx, start)) = logger() {
        let _ = tx.send(format!("{:>8} ms  {}", start.elapsed().as_millis(), line()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_value_one_enables_the_log() {
        assert!(is_requested(Some(OsStr::new("1"))));
        for value in ["", "0", "true", "yes", "11", " 1", "1 "] {
            assert!(!is_requested(Some(OsStr::new(value))), "{value:?}");
        }
        assert!(!is_requested(None));
    }
}
