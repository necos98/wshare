//! The real clipboard: `CF_UNICODETEXT` through the Win32 clipboard API.

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::core::w;

use super::access::{AccessError, ClipboardAccess};
use super::encoding;
use super::rules::ClipboardContent;
use super::window::ClipboardOwner;

/// The format programs that keep secrets (password managers) put on the
/// clipboard to ask not to be monitored, clipboard history included.
const EXCLUDE_FROM_MONITORING: windows::core::PCWSTR =
    w!("ExcludeClipboardContentFromMonitorProcessing");

const UNIT_BYTES: usize = size_of::<u16>();

pub struct SystemClipboard {
    owner: ClipboardOwner,
}

impl SystemClipboard {
    /// The clipboard, opened on behalf of `owner`'s window.
    pub fn owned_by(owner: ClipboardOwner) -> Self {
        Self { owner }
    }

    /// Opens the clipboard; it closes when the result is dropped. Failing
    /// means another program has it open, which is worth retrying.
    fn open(&self) -> Result<OpenClipboardGuard, AccessError> {
        // SAFETY: the window handle is only used as the owner; a stale one
        // makes the call fail.
        unsafe { OpenClipboard(Some(self.owner.handle())) }
            .map(|()| OpenClipboardGuard)
            .map_err(|_| AccessError::Busy)
    }
}

/// The clipboard is open while this exists.
struct OpenClipboardGuard;

impl Drop for OpenClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: made only after a successful `OpenClipboard` on this
        // thread.
        let _ = unsafe { CloseClipboard() };
    }
}

impl ClipboardAccess for SystemClipboard {
    fn read(&self) -> Result<ClipboardContent, AccessError> {
        let _open = self.open()?;
        // SAFETY: querying formats needs the clipboard open, which it is.
        let has_text = unsafe { IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT.0)) }.is_ok();
        if !has_text {
            return Ok(ClipboardContent::NoText);
        }
        if excluded_from_monitoring() {
            return Ok(ClipboardContent::Private);
        }
        // SAFETY: the clipboard is open; the handle stays owned by it.
        let handle = unsafe { GetClipboardData(u32::from(CF_UNICODETEXT.0)) }
            .map_err(|e| failed("reading the clipboard", &e))?;
        // SAFETY: `handle` is the global memory of CF_UNICODETEXT, valid
        // until the clipboard closes, which is after this call.
        unsafe { read_units(HGLOBAL(handle.0)) }
    }

    fn write(&self, text: &str) -> Result<(), AccessError> {
        let units = encoding::encode(text);
        let _open = self.open()?;
        // SAFETY: the clipboard is open.
        unsafe { EmptyClipboard() }.map_err(|e| failed("emptying the clipboard", &e))?;
        let memory = alloc_units(&units)?;
        // SAFETY: the clipboard is open and `memory` is a movable block
        // holding NUL-terminated UTF-16, as CF_UNICODETEXT requires.
        let stored =
            unsafe { SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(memory.0))) };
        match stored {
            // The clipboard owns the memory now.
            Ok(_) => Ok(()),
            Err(e) => {
                // SAFETY: the clipboard did not take `memory`, so it is ours
                // to free.
                let _ = unsafe { GlobalFree(Some(memory)) };
                Err(failed("writing the clipboard", &e))
            }
        }
    }
}

fn failed(what: &str, error: &windows::core::Error) -> AccessError {
    AccessError::Failed(format!("{what}: {error}"))
}

fn excluded_from_monitoring() -> bool {
    // SAFETY: registering a named format and asking for it have no
    // preconditions beyond the clipboard being open for the second call.
    unsafe {
        let format = RegisterClipboardFormatW(EXCLUDE_FROM_MONITORING);
        format != 0 && IsClipboardFormatAvailable(format).is_ok()
    }
}

/// The text in the clipboard memory `memory`.
///
/// # Safety
/// `memory` must be a valid global memory handle that stays valid for the
/// call, holding UTF-16 (the clipboard guarantees this for CF_UNICODETEXT).
unsafe fn read_units(memory: HGLOBAL) -> Result<ClipboardContent, AccessError> {
    // SAFETY: per the contract.
    let (pointer, size) = unsafe { (GlobalLock(memory), GlobalSize(memory)) };
    if pointer.is_null() {
        return Err(AccessError::Failed("locking the clipboard memory".into()));
    }
    // SAFETY: the block is locked, `size` bytes long, and was allocated
    // with at least 2-byte alignment (global memory is 8 or 16-aligned).
    let units = unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), size / UNIT_BYTES) };
    let content = encoding::decode(units);
    // Fails with "no error" when the lock count reaches zero: nothing to
    // handle.
    // SAFETY: pairs the `GlobalLock` above.
    let _ = unsafe { GlobalUnlock(memory) };
    Ok(content)
}

/// A movable block of global memory holding `units`.
fn alloc_units(units: &[u16]) -> Result<HGLOBAL, AccessError> {
    let bytes = units.len() * UNIT_BYTES;
    // SAFETY: allocating has no preconditions.
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }
        .map_err(|e| failed("allocating clipboard memory", &e))?;
    // SAFETY: `memory` is a fresh block of `bytes` bytes, locked here for
    // the copy and unlocked after it.
    unsafe {
        let pointer = GlobalLock(memory).cast::<u16>();
        if pointer.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err(AccessError::Failed("locking the clipboard memory".into()));
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len());
        let _ = GlobalUnlock(memory);
    }
    Ok(memory)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::access::Retry;
    use super::super::window::ClipboardWindow;
    use super::*;

    const WAIT: Duration = Duration::from_secs(5);

    /// Writes and reads the machine's real clipboard, so it only runs on
    /// request (`cargo test -- --ignored`) and leaves the user's clipboard
    /// replaced.
    #[tokio::test]
    #[ignore = "replaces the contents of the real clipboard"]
    async fn text_written_is_read_back_and_announced() {
        let (window, mut changes) = ClipboardWindow::open().expect("listener");
        let clipboard = SystemClipboard::owned_by(window.owner());
        let text = "wshare è un test \u{1F5A5}\r\nseconda riga";

        clipboard.write(text).expect("write");

        let announced = tokio::time::timeout(WAIT, changes.recv()).await;
        assert_eq!(announced, Ok(Some(())), "the change was announced");
        // Other programs (clipboard history) open it right after a change.
        let read = Retry::default().run(|| clipboard.read()).await;
        assert_eq!(read, Ok(ClipboardContent::Text(text.to_string())));
    }
}
