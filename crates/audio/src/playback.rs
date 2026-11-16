//! Playback on the default device.

use anyhow::{Context, Result};
use protocol::audio::{CHANNELS, FRAME_BYTES};
use windows::Win32::Media::Audio::{IAudioClient, IAudioRenderClient};

use crate::device::{DefaultDevice, device_id, open_client};

/// The device buffer. The player keeps far less than this queued (see
/// [`Playback::queued_frames`]); the rest is room for a late wake-up.
const BUFFER_MS: i64 = 100;

/// A running playback stream on the default device.
pub struct Playback {
    client: IAudioClient,
    render: IAudioRenderClient,
    capacity: u32,
    device: String,
    /// Reused by every write: the samples before they are copied into the
    /// device buffer.
    scratch: Vec<i16>,
}

impl Playback {
    /// Starts playing (silence until written) on the default device; fails
    /// when there is none.
    pub fn open(devices: &DefaultDevice) -> Result<Self> {
        let device = devices.device()?;
        let id = device_id(&device)?;
        let client = open_client(&device, 0, BUFFER_MS)?;
        // SAFETY: initialised client, plain calls.
        let capacity = unsafe { client.GetBufferSize() }.context("reading the buffer size")?;
        let render: IAudioRenderClient =
            unsafe { client.GetService() }.context("getting the render client")?;
        unsafe { client.Start() }.context("starting playback")?;
        Ok(Self {
            client,
            render,
            capacity,
            device: id,
            scratch: Vec::new(),
        })
    }

    /// The device played on.
    pub fn device(&self) -> &str {
        &self.device
    }

    /// Frames written and not played yet.
    pub fn queued_frames(&self) -> Result<u32> {
        // SAFETY: started client, plain call.
        unsafe { self.client.GetCurrentPadding() }.context("reading the playback queue")
    }

    /// Frames that can be written now.
    pub fn free_frames(&self) -> Result<u32> {
        Ok(self.capacity.saturating_sub(self.queued_frames()?))
    }

    /// Queues `frames` frames (at most [`Self::free_frames`]), produced by
    /// `fill` into an interleaved buffer.
    pub fn write(&mut self, frames: u32, fill: impl FnOnce(&mut [i16])) -> Result<()> {
        if frames == 0 {
            return Ok(());
        }
        self.scratch.clear();
        self.scratch.resize(frames as usize * CHANNELS, 0);
        fill(&mut self.scratch);
        // SAFETY: plain call; the buffer holds `frames` frames until the
        // release below.
        let data =
            unsafe { self.render.GetBuffer(frames) }.context("getting the playback buffer")?;
        // SAFETY: written as bytes, so alignment does not matter; exactly
        // the `frames` frames obtained above.
        let out = unsafe { std::slice::from_raw_parts_mut(data, frames as usize * FRAME_BYTES) };
        for (pair, sample) in out.chunks_exact_mut(2).zip(&self.scratch) {
            pair.copy_from_slice(&sample.to_le_bytes());
        }
        // SAFETY: releases exactly the frames obtained above.
        unsafe { self.render.ReleaseBuffer(frames, 0) }.context("releasing the playback buffer")
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        // SAFETY: plain call; a device already gone has nothing to stop.
        let _ = unsafe { self.client.Stop() };
    }
}
