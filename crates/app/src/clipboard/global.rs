//! Movable global memory, the way the clipboard and OLE hand data around.

use windows::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

use super::access::AccessError;

/// A movable block of global memory holding a copy of `bytes`. Whoever it
/// is handed to (the clipboard, OLE's caller) frees it.
pub fn alloc_bytes(bytes: &[u8]) -> Result<HGLOBAL, AccessError> {
    // SAFETY: allocating has no preconditions.
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }
        .map_err(|e| AccessError::Failed(format!("allocating clipboard memory: {e}")))?;
    // SAFETY: `memory` is a fresh block of `bytes.len()` bytes, locked here
    // for the copy and unlocked after it.
    unsafe {
        let pointer = GlobalLock(memory).cast::<u8>();
        if pointer.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err(AccessError::Failed("locking the clipboard memory".into()));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
        // Fails with "no error" when the lock count reaches zero.
        let _ = GlobalUnlock(memory);
    }
    Ok(memory)
}
