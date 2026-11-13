//! The host's frame source: desktop capture followed by NV12 conversion.

use std::time::Duration;

use anyhow::Result;
use capture::{Acquire, DesktopDuplication, PointerChanges};
use gpu::Gpu;
use pipeline::FrameSource;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

use crate::BgraToNv12;

/// Captures a monitor and hands out encoder-ready NV12 textures.
///
/// It composes the two GPU stages rather than extending either, so capture
/// stays unaware of encoding and the converter of where its input comes from.
pub struct DesktopSource {
    capture: DesktopDuplication,
    converter: BgraToNv12,
}

impl DesktopSource {
    /// Opens monitor `monitor_index`; `fps` tunes the video processor.
    pub fn open(monitor_index: usize, fps: u32) -> Result<Self> {
        let capture = DesktopDuplication::open(monitor_index)?;
        let converter = BgraToNv12::new(
            capture.gpu(),
            capture.texture(),
            capture.width,
            capture.height,
            fps,
        )?;
        Ok(Self { capture, converter })
    }

    /// The device the frames live on (create the encoder on it).
    pub fn gpu(&self) -> &Gpu {
        self.capture.gpu()
    }

    pub fn width(&self) -> u32 {
        self.capture.width
    }

    pub fn height(&self) -> u32 {
        self.capture.height
    }

    /// Pointer shape/visibility changes since the previous call, see
    /// [`DesktopDuplication::take_pointer_changes`]. They are gathered while
    /// waiting for frames, so poll this between streamer steps.
    pub fn take_pointer_changes(&mut self) -> PointerChanges {
        self.capture.take_pointer_changes()
    }
}

impl FrameSource for DesktopSource {
    type Frame = ID3D11Texture2D;

    fn wait_for_change(&mut self, timeout: Duration) -> Result<bool> {
        // Round up: truncating 0.6 ms to 0 would turn the wait into a spin.
        let timeout_ms = u32::try_from(timeout.as_micros().div_ceil(1000)).unwrap_or(u32::MAX);
        Ok(matches!(
            self.capture.acquire(timeout_ms)?,
            Acquire::NewFrame
        ))
    }

    /// Converts the latest desktop image into the next pooled NV12 surface.
    fn current_frame(&mut self) -> Result<ID3D11Texture2D> {
        self.converter.convert()
    }
}
