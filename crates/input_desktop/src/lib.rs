//! Following the input desktop.
//!
//! Windows shows the lock screen, the sign-in screen and the UAC prompts on
//! the secure desktop (`Winlogon`), not on the user's (`Default`). A thread
//! captures the screen and injects input only on the desktop it is attached
//! to, so the unattended-access agent (a SYSTEM process, the only kind that
//! may open the secure desktop) re-attaches its capture and input threads
//! to whichever desktop receives the input right now.
//!
//! Off unless a process turns it on with [`follow_input_desktop`]: an
//! ordinary wshare process never changes the desktop of its threads.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use windows::Win32::Foundation::GENERIC_ALL;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, HDESK, OpenInputDesktop,
    SetThreadDesktop,
};

/// Whether this process follows the input desktop.
static FOLLOWING: AtomicBool = AtomicBool::new(false);

/// Capture (DXGI) and input (`SendInput`) need most rights on the desktop;
/// only a SYSTEM process can be granted them on the secure desktop.
const DESKTOP_RIGHTS: DESKTOP_ACCESS_FLAGS = DESKTOP_ACCESS_FLAGS(GENERIC_ALL.0);

thread_local! {
    /// The desktop this thread was attached to, kept open while in use and
    /// closed once the thread moves on.
    static ATTACHED: Cell<Option<HDESK>> = const { Cell::new(None) };
}

/// Makes the threads of this process follow the input desktop from now on
/// (see [`attach_to_input_desktop`]). For the unattended-access agent only.
pub fn follow_input_desktop() {
    FOLLOWING.store(true, Ordering::Relaxed);
}

/// Whether [`follow_input_desktop`] was called in this process.
pub fn is_following() -> bool {
    FOLLOWING.load(Ordering::Relaxed)
}

/// When this process follows the input desktop, attaches the calling
/// thread to it; otherwise does nothing. `Ok(true)`: the thread is now on
/// the input desktop.
///
/// Fails if Windows does not let this process open the input desktop, or
/// the thread owns windows or hooks (it cannot change desktop then).
pub fn attach_to_input_desktop() -> Result<bool> {
    if !is_following() {
        return Ok(false);
    }
    // SAFETY: plain Win32 calls; the new handle is either kept as this
    // thread's desktop or closed here, and the previous one is closed only
    // once the thread no longer uses it.
    unsafe {
        let desktop = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_RIGHTS)
            .context("opening the input desktop")?;
        if let Err(e) = SetThreadDesktop(desktop) {
            let _ = CloseDesktop(desktop);
            return Err(e).context("moving the thread to the input desktop");
        }
        if let Some(previous) = ATTACHED.replace(Some(desktop)) {
            let _ = CloseDesktop(previous);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_process_never_moves_its_threads() {
        // Tests run in an ordinary process: nothing turned following on.
        assert!(!is_following());
        assert!(!attach_to_input_desktop().unwrap());
    }
}
