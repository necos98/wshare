//! Process-wide Media Foundation start-up.

use std::marker::PhantomData;

use anyhow::{Context, Result};
use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_LITE, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// Keeps COM (multithreaded apartment) and Media Foundation initialised on
/// the current thread while alive.
///
/// MF reference-counts `MFStartup`, so several guards may coexist. COM is
/// per-thread, hence the guard is `!Send`: it must be dropped on the thread
/// that created it.
pub struct MediaFoundation {
    uninit_com: bool,
    _not_send: PhantomData<*const ()>,
}

impl MediaFoundation {
    pub fn start() -> Result<Self> {
        // S_OK / S_FALSE both need a matching CoUninitialize. A failure
        // (thread already in an STA) is fine: MF works in either apartment.
        let uninit_com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        // LITE: we never use MF's network sources.
        if let Err(e) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) } {
            if uninit_com {
                unsafe { CoUninitialize() };
            }
            return Err(e).context("MFStartup");
        }
        Ok(Self {
            uninit_com,
            _not_send: PhantomData,
        })
    }
}

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            if self.uninit_com {
                CoUninitialize();
            }
        }
    }
}
