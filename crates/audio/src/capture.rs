//! Loopback capture: what the default playback device is playing.

use anyhow::{Context, Result};
use protocol::audio::FRAME_BYTES;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient, IAudioClient,
};

use crate::device::{DefaultDevice, device_id, open_client};

/// The capture buffer: how late the reader may be before Windows drops
/// sound. The reader drains it every few milliseconds, so this adds no
/// delay.
const BUFFER_MS: i64 = 200;

/// A running loopback capture of the default playback device. Windows
/// delivers nothing while nothing plays.
pub struct LoopbackCapture {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    device: String,
}

impl LoopbackCapture {
    /// Starts capturing the default playback device; fails when there is
    /// none or it cannot be captured.
    pub fn open(devices: &DefaultDevice) -> Result<Self> {
        let device = devices.device()?;
        let id = device_id(&device)?;
        let client = open_client(&device, AUDCLNT_STREAMFLAGS_LOOPBACK, BUFFER_MS)?;
        // SAFETY: initialised client, plain calls.
        let capture: IAudioCaptureClient =
            unsafe { client.GetService() }.context("getting the capture client")?;
        unsafe { client.Start() }.context("starting the loopback capture")?;
        Ok(Self {
            client,
            capture,
            device: id,
        })
    }

    /// The device captured.
    pub fn device(&self) -> &str {
        &self.device
    }

    /// Appends every frame captured since the last call to `out`
    /// (interleaved samples). Fails when the device went away or changed
    /// its format: the capture must then be opened again.
    pub fn read(&mut self, out: &mut Vec<i16>) -> Result<()> {
        loop {
            // SAFETY: started client, plain call.
            let waiting = unsafe { self.capture.GetNextPacketSize() }
                .context("reading the capture packet size")?;
            if waiting == 0 {
                return Ok(());
            }
            self.take_packet(out)?;
        }
    }

    fn take_packet(&mut self, out: &mut Vec<i16>) -> Result<()> {
        let mut data = std::ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        // SAFETY: the out-pointers are valid for the call; the buffer it
        // returns holds `frames` frames until `ReleaseBuffer`.
        unsafe {
            self.capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
        }
        .context("reading the capture buffer")?;
        let bytes = frames as usize * FRAME_BYTES;
        let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
        if silent || data.is_null() {
            out.resize(out.len() + bytes / 2, 0);
        } else {
            // SAFETY: WASAPI guarantees `bytes` readable bytes at `data`
            // until the release below; read as bytes, so alignment does
            // not matter.
            let pcm = unsafe { std::slice::from_raw_parts(data.cast_const(), bytes) };
            out.extend(
                pcm.chunks_exact(2)
                    .map(|sample| i16::from_le_bytes([sample[0], sample[1]])),
            );
        }
        // SAFETY: releases exactly the packet obtained above.
        unsafe { self.capture.ReleaseBuffer(frames) }.context("releasing the capture buffer")
    }
}

impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        // SAFETY: plain call; a device already gone has nothing to stop.
        let _ = unsafe { self.client.Stop() };
    }
}
