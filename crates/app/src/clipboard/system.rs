//! The real clipboard: `CF_UNICODETEXT` and `CF_HDROP` through the Win32
//! clipboard API, the other PC's files through OLE (see `window`).

use std::path::PathBuf;

use protocol::transfer::MAX_TRANSFER_ENTRIES;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::core::w;

use super::access::{AccessError, ClipboardAccess};
use super::encoding;
use super::global::alloc_bytes;
use super::rules::ClipboardContent;
use super::virtual_files::RemoteFiles;
use super::window::{ClipboardOwner, ClipboardWindow, FileOffers};

/// The format programs that keep secrets (password managers) put on the
/// clipboard to ask not to be monitored, clipboard history included.
const EXCLUDE_FROM_MONITORING: windows::core::PCWSTR =
    w!("ExcludeClipboardContentFromMonitorProcessing");

const UNIT_BYTES: usize = size_of::<u16>();

pub struct SystemClipboard {
    owner: ClipboardOwner,
    offers: FileOffers,
}

impl SystemClipboard {
    /// The clipboard, opened on behalf of `window`, which also puts the
    /// other PC's files on it.
    pub fn owned_by(window: &ClipboardWindow) -> Self {
        Self {
            owner: window.owner(),
            offers: window.file_offers(),
        }
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
        let (has_files, has_text) = unsafe {
            (
                IsClipboardFormatAvailable(u32::from(CF_HDROP.0)).is_ok(),
                IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT.0)).is_ok(),
            )
        };
        if !has_files && !has_text {
            return Ok(ClipboardContent::NoText);
        }
        if excluded_from_monitoring() {
            return Ok(ClipboardContent::Private);
        }
        if has_files {
            // SAFETY: the clipboard is open; the handle stays owned by it.
            let handle = unsafe { GetClipboardData(u32::from(CF_HDROP.0)) }
                .map_err(|e| failed("reading the copied files", &e))?;
            // SAFETY: `handle` is the HDROP of CF_HDROP, valid until the
            // clipboard closes, which is after this call.
            return Ok(ClipboardContent::Files(unsafe {
                dropped_files(HDROP(handle.0))
            }));
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
        // SAFETY: the units, seen as their bytes.
        let bytes = unsafe {
            std::slice::from_raw_parts(units.as_ptr().cast::<u8>(), units.len() * UNIT_BYTES)
        };
        let memory = alloc_bytes(bytes)?;
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

    fn offer_files(&self, files: RemoteFiles) -> Result<(), AccessError> {
        self.offers.offer(files)
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

/// The paths in `drop`: at most one more than a list may hold (so a list
/// too long is still recognised as such), each as Windows gave it.
///
/// # Safety
/// `drop` must be a valid HDROP that stays valid for the call.
unsafe fn dropped_files(drop: HDROP) -> Vec<PathBuf> {
    // SAFETY: per the contract; index 0xFFFFFFFF asks for the count.
    let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
    let wanted = count.min(u32::try_from(MAX_TRANSFER_ENTRIES + 1).unwrap_or(u32::MAX));
    (0..wanted)
        .filter_map(|index| {
            // SAFETY: per the contract; `index` < count. The first call
            // gives the length without the terminator.
            let len = unsafe { DragQueryFileW(drop, index, None) } as usize;
            let mut units = vec![0u16; len + 1];
            // SAFETY: as above, into a buffer of `len + 1` units.
            let copied = unsafe { DragQueryFileW(drop, index, Some(&mut units)) } as usize;
            (copied > 0).then(|| PathBuf::from(String::from_utf16_lossy(&units[..copied])))
        })
        .collect()
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
        let clipboard = SystemClipboard::owned_by(&window);
        let text = "wshare è un test \u{1F5A5}\r\nseconda riga";

        clipboard.write(text).expect("write");

        let announced = tokio::time::timeout(WAIT, changes.recv()).await;
        assert_eq!(announced, Ok(Some(())), "the change was announced");
        // Other programs (clipboard history) open it right after a change.
        let read = Retry::default().run(|| clipboard.read()).await;
        assert_eq!(read, Ok(ClipboardContent::Text(text.to_string())));
    }
}
