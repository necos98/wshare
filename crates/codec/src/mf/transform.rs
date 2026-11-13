//! Calls on `IMFTransform` whose ownership rules are easy to get wrong.

use std::mem::ManuallyDrop;

use windows::Win32::Media::MediaFoundation::{
    IMFSample, IMFShutdown, IMFTransform, MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_OUTPUT_DATA_BUFFER,
};
use windows::core::Interface;

/// Calls `ProcessOutput` on stream 0. `provided` is the caller-allocated
/// sample, for MFTs that do not provide their own. Returns the call result
/// and whatever sample ended up in the output buffer, so nothing leaks.
pub(crate) fn process_output(
    transform: &IMFTransform,
    provided: Option<IMFSample>,
) -> (windows::core::Result<()>, Option<IMFSample>) {
    let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
        dwStreamID: 0,
        pSample: ManuallyDrop::new(provided),
        dwStatus: 0,
        pEvents: ManuallyDrop::new(None),
    }];
    let mut status = 0u32;
    let result = unsafe { transform.ProcessOutput(0, &mut buffers, &mut status) };
    // SAFETY: each field is taken exactly once; we own the references stored
    // there (by us or by the MFT) and dropping them releases them.
    let sample = unsafe { ManuallyDrop::take(&mut buffers[0].pSample) };
    drop(unsafe { ManuallyDrop::take(&mut buffers[0].pEvents) });
    (result, sample)
}

/// Ends streaming and releases the MFT's resources (worker threads, D3D
/// objects) now rather than whenever the last COM reference goes away.
pub(crate) fn shutdown(transform: &IMFTransform) {
    unsafe {
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        if let Ok(shutdown) = transform.cast::<IMFShutdown>() {
            let _ = shutdown.Shutdown();
        }
    }
}
