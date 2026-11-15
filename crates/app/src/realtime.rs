//! Scheduling boosts for the threads on the frame path.
//!
//! Both are best effort: a failure only means the default scheduling, never
//! an error for the session, and each is undone when its guard is dropped.

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::{TIMERR_NOERROR, timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
};
use windows::core::{PCWSTR, w};

/// Timer resolution requested while streaming. The default (about 15.6 ms)
/// would round every capture wait up to a coarse tick and cost up to a frame
/// of latency.
const TIMER_RESOLUTION_MS: u32 = 1;

/// MMCSS task class of the frame-path threads: short, periodic,
/// latency-sensitive work.
const MMCSS_TASK: PCWSTR = w!("Games");

/// Raises the system timer resolution for as long as it lives.
pub struct TimerResolution {
    raised: bool,
}

impl TimerResolution {
    pub fn raise() -> Self {
        // SAFETY: plain call, paired with `timeEndPeriod` in `Drop`.
        let raised = unsafe { timeBeginPeriod(TIMER_RESOLUTION_MS) } == TIMERR_NOERROR;
        Self { raised }
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        if self.raised {
            // SAFETY: matches the successful `timeBeginPeriod` above.
            unsafe { timeEndPeriod(TIMER_RESOLUTION_MS) };
        }
    }
}

/// Registers the calling thread with MMCSS for as long as it lives. Create
/// and drop it on the same thread.
pub struct MmcssRegistration {
    handle: Option<HANDLE>,
}

impl MmcssRegistration {
    pub fn register() -> Self {
        let mut task_index = 0u32;
        // SAFETY: the task name is a static NUL-terminated string and `task_index`
        // outlives the call.
        let handle = unsafe { AvSetMmThreadCharacteristicsW(MMCSS_TASK, &mut task_index) }.ok();
        Self { handle }
    }
}

impl Drop for MmcssRegistration {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: `handle` came from `AvSetMmThreadCharacteristicsW` on
            // this thread. A failed revert leaves nothing to recover.
            let _ = unsafe { AvRevertMmThreadCharacteristics(handle) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guards_can_be_created_and_dropped() {
        let timer = TimerResolution::raise();
        let mmcss = MmcssRegistration::register();
        // Whether MMCSS is available depends on the machine; either way the
        // guards must clean up without panicking.
        drop(mmcss);
        drop(timer);
    }
}
