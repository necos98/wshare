//! Windows glue made necessary by the GUI: wshare.exe is a "windows"
//! subsystem program (no console window on double-click), so command-line
//! commands must borrow the console of the terminal that started them, and
//! fatal GUI errors are shown in a message box.

use std::os::windows::io::IntoRawHandle;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::HSTRING;

/// Makes `println!`/`eprintln!`/stdin work for a command started from a
/// terminal: attaches to the parent's console and points the standard
/// handles that are missing at it. Handles that are already valid (output
/// redirected to a file or pipe, e.g. a viewer started by the GUI) are kept.
pub fn attach_parent_console() {
    // SAFETY: plain Win32 calls; failure only means there is no parent console.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_err() {
        return;
    }
    for (handle, device, write) in [
        (STD_INPUT_HANDLE, "CONIN$", false),
        (STD_OUTPUT_HANDLE, "CONOUT$", true),
        (STD_ERROR_HANDLE, "CONOUT$", true),
    ] {
        if has_handle(handle) {
            continue;
        }
        let file = std::fs::OpenOptions::new().read(true).write(write).open(device);
        if let Ok(file) = file {
            // The handle stays open for the life of the process.
            let raw = HANDLE(file.into_raw_handle());
            // SAFETY: `raw` is a valid console handle we own and never close.
            let _ = unsafe { SetStdHandle(handle, raw) };
        }
    }
}

fn has_handle(which: STD_HANDLE) -> bool {
    // SAFETY: GetStdHandle has no preconditions.
    unsafe { GetStdHandle(which) }.is_ok_and(|h| !h.is_invalid() && !h.0.is_null())
}

/// Shows a blocking error dialog (the GUI has no console to print to).
pub fn error_box(message: &str) {
    // SAFETY: both strings outlive the call; no owner window.
    unsafe {
        MessageBoxW(None, &HSTRING::from(message), &HSTRING::from("wshare"), MB_OK | MB_ICONERROR);
    }
}
