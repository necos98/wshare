//! Ctrl+C / Ctrl+Break / console close for the CLI host.
//!
//! Without a handler Windows kills the process on Ctrl+C: keys the viewer
//! holds would stay pressed and the viewer would not be told Bye. The
//! handler only signals; the CLI then stops the service normally.
//!
//! Declared directly against kernel32 (always linked on Windows) to avoid
//! needing tokio's `signal` feature or another `windows` crate feature.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Result, bail};

const CTRL_C_EVENT: u32 = 0;
const CTRL_BREAK_EVENT: u32 = 1;
const CTRL_CLOSE_EVENT: u32 = 2;
const TRUE: i32 = 1;
const FALSE: i32 = 0;

/// Windows ends the process this long after a close event at the latest;
/// waiting in the handler gives the stop that much time to finish.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

type HandlerRoutine = unsafe extern "system" fn(ctrl_type: u32) -> i32;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<HandlerRoutine>, add: i32) -> i32;
}

static CALLBACK: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Calls `on_interrupt` (on a system thread) the first time the user asks
/// the console program to end. A second Ctrl+C falls through to Windows'
/// default (immediate exit), as an escape hatch should stopping hang.
/// Can be installed once per process.
pub(super) fn install(on_interrupt: impl Fn() + Send + Sync + 'static) -> Result<()> {
    if CALLBACK.set(Box::new(on_interrupt)).is_err() {
        bail!("console interrupt handler already installed");
    }
    // SAFETY: `handler` is a plain `extern "system"` fn that lives for the
    // whole program and only touches statics.
    if unsafe { SetConsoleCtrlHandler(Some(handler), TRUE) } == 0 {
        bail!(
            "SetConsoleCtrlHandler failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

unsafe extern "system" fn handler(ctrl_type: u32) -> i32 {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            if INTERRUPTED.swap(true, Ordering::SeqCst) {
                return FALSE; // second time: let Windows end the process
            }
            notify();
            TRUE
        }
        CTRL_CLOSE_EVENT => {
            notify();
            // Returning lets Windows end the process right away; the main
            // thread exits the process itself once the stop is done.
            std::thread::sleep(CLOSE_WAIT);
            TRUE
        }
        _ => FALSE,
    }
}

fn notify() {
    if let Some(callback) = CALLBACK.get() {
        callback();
    }
}
