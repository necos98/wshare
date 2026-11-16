//! COM on the audio threads.

use std::marker::PhantomData;

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// Keeps COM initialised (multithreaded apartment) on the current thread
/// while alive. `!Send`: dropped on the thread that created it.
pub struct ComApartment {
    uninit: bool,
    _not_send: PhantomData<*const ()>,
}

impl ComApartment {
    /// Enters the multithreaded apartment. A thread already in another
    /// apartment keeps it: WASAPI works in either.
    pub fn enter() -> Self {
        // SAFETY: plain call; S_OK and S_FALSE are paired with
        // `CoUninitialize` in `Drop`, a failure is not.
        let uninit = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        Self {
            uninit,
            _not_send: PhantomData,
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.uninit {
            // SAFETY: matches the successful `CoInitializeEx` on this thread.
            unsafe { CoUninitialize() };
        }
    }
}
