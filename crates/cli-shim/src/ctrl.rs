//! Keeps Ctrl+C / Ctrl+Break from ending the shim while wshare.exe runs.
//!
//! A handler that reports "handled" is used rather than
//! `SetConsoleCtrlHandler(NULL, TRUE)`: that "ignore Ctrl+C" flag is
//! inherited by child processes and would stop wshare.exe from seeing
//! Ctrl+C, while a handler is not inherited. Console-close and logoff
//! events still end the shim normally (wshare.exe gets its own).
//!
//! Declared directly against kernel32 to stay dependency-free.

const CTRL_C_EVENT: u32 = 0;
const CTRL_BREAK_EVENT: u32 = 1;
const TRUE: i32 = 1;
const FALSE: i32 = 0;

type HandlerRoutine = unsafe extern "system" fn(ctrl_type: u32) -> i32;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<HandlerRoutine>, add: i32) -> i32;
}

/// Installs the handler. If that fails the shim can still run; it would
/// only end on Ctrl+C together with the child, as before.
pub fn ignore_interrupts() {
    // SAFETY: `handler` is a plain `extern "system"` fn that lives for the
    // whole program and touches nothing.
    unsafe { SetConsoleCtrlHandler(Some(handler), TRUE) };
}

/// Whether the shim swallows this console control event.
fn swallows(ctrl_type: u32) -> bool {
    matches!(ctrl_type, CTRL_C_EVENT | CTRL_BREAK_EVENT)
}

unsafe extern "system" fn handler(ctrl_type: u32) -> i32 {
    if swallows(ctrl_type) { TRUE } else { FALSE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupts_are_swallowed_close_is_not() {
        assert!(swallows(CTRL_C_EVENT));
        assert!(swallows(CTRL_BREAK_EVENT));
        assert!(!swallows(2), "CTRL_CLOSE_EVENT");
        assert!(!swallows(5), "CTRL_LOGOFF_EVENT");
    }
}
