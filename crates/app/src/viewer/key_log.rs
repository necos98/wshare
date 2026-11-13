//! Opt-in keyboard diagnostics: with `WSHARE_DEBUG_KEYS=1` every line is
//! appended to `wshare-keys.log` on the user's desktop.
//!
//! Writing happens on a background thread: the keyboard hook must never
//! block (Windows skips hooks that are slow to answer), and a terminal pipe
//! can stall for a long time.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::time::Instant;

static LOG: OnceLock<Option<(Sender<String>, Instant)>> = OnceLock::new();

fn logger() -> Option<&'static (Sender<String>, Instant)> {
    LOG.get_or_init(|| {
        std::env::var_os("WSHARE_DEBUG_KEYS")?;
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
    let dir = log_dir();
    let candidates = [
        dir.join("wshare-keys.log"),
        dir.join(format!("wshare-keys-{}.log", std::process::id())),
    ];
    for path in candidates {
        if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
            return Some((path, file));
        }
    }
    eprintln!("Keyboard diagnostics disabled: cannot create a log in {}", dir.display());
    None
}

fn log_dir() -> PathBuf {
    let base = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    let desktop = base.join("Desktop");
    if desktop.is_dir() { desktop } else { std::env::temp_dir() }
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
