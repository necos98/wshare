//! The injection boundary: the only place that calls `SendInput`.

use anyhow::Result;
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, SendInput};

use crate::synth::SyntheticInput;

/// Injects events into the desktop.
pub trait InputBackend {
    /// Injects `inputs` in order; returns how many were injected.
    ///
    /// Fewer than `inputs.len()` is not an error: it is how Windows reports
    /// UIPI / secure-desktop blocking. `Err` is for real failures only.
    fn send(&mut self, inputs: &[SyntheticInput]) -> Result<u32>;
}

/// The real backend (Win32 `SendInput`). Safe to use from any thread of the
/// interactive desktop; it keeps a scratch buffer to avoid allocating per
/// event.
#[derive(Default)]
pub struct SendInputBackend {
    buffer: Vec<INPUT>,
}

impl SendInputBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

impl InputBackend for SendInputBackend {
    fn send(&mut self, inputs: &[SyntheticInput]) -> Result<u32> {
        if inputs.is_empty() {
            return Ok(0);
        }
        self.buffer.clear();
        self.buffer.extend(inputs.iter().map(|i| i.to_win32()));
        let size = i32::try_from(size_of::<INPUT>())?;
        // SAFETY: the slice holds fully initialized INPUTs whose `type`
        // matches the union variant written by `to_win32`, and `size` is
        // the real struct size. SendInput only reads the buffer.
        // UIPI blocking yields a short count with no reliable last-error
        // (documented by Microsoft), so the count is the whole answer.
        Ok(unsafe { SendInput(&self.buffer, size) })
    }
}
