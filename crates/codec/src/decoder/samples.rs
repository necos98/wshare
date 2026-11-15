//! Moving data in and out of the decoder: compressed input samples,
//! caller-allocated output samples, and decoded samples -> [`GpuFrame`].

use anyhow::{Context, Result};
use gpu::GpuFrame;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_DECODER, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::core::Interface;

/// Smallest input buffer allocated, and the granularity its size grows in,
/// so a stream of similar-sized frames settles on one buffer.
const INPUT_BUFFER_STEP: u32 = 64 * 1024;

/// Wraps access units in system-memory samples, reusing one Media Foundation
/// buffer from frame to frame instead of allocating a new one each time.
#[derive(Default)]
pub(super) struct InputSamples {
    buffer: Option<IMFMediaBuffer>,
}

impl InputSamples {
    /// Wraps one Annex-B access unit in a sample.
    pub(super) fn sample(&mut self, unit: &[u8], timestamp: i64) -> Result<IMFSample> {
        let len = u32::try_from(unit.len()).context("access unit too large")?;
        let buffer = self.buffer_for(len)?;
        unsafe {
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            // SAFETY: `buffer_for` guarantees room for `len` bytes.
            std::ptr::copy_nonoverlapping(unit.as_ptr(), ptr, unit.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(len)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(timestamp)?;
            Ok(sample)
        }
    }

    /// The cached buffer if it is big enough and nobody else (the decoder
    /// may keep an input sample until it has used it) still refers to it;
    /// otherwise a new one that becomes the cache.
    fn buffer_for(&mut self, len: u32) -> Result<IMFMediaBuffer> {
        if let Some(buffer) = &self.buffer
            && is_unshared(buffer)
            && unsafe { buffer.GetMaxLength() }? >= len
        {
            return Ok(buffer.clone());
        }
        let capacity = len.div_ceil(INPUT_BUFFER_STEP).max(1) * INPUT_BUFFER_STEP;
        let buffer = unsafe { MFCreateMemoryBuffer(capacity)? };
        self.buffer = Some(buffer.clone());
        Ok(buffer)
    }
}

/// True when the cache holds the only reference to `buffer`.
fn is_unshared(buffer: &IMFMediaBuffer) -> bool {
    // SAFETY: `buffer` is a live COM object; the probe's AddRef/Release pair
    // leaves the count unchanged.
    let references = unsafe {
        let vtable = Interface::vtable(buffer);
        let raw = buffer.as_raw();
        let references = (vtable.base__.AddRef)(raw);
        (vtable.base__.Release)(raw);
        references
    };
    // The cache's own reference, plus the probe's.
    references == 2
}
/// An output sample backed by a new NV12 texture, for decoders that do not
/// provide their own (`MFT_OUTPUT_STREAM_PROVIDES_SAMPLES` unset). The MS
/// decoders always provide samples once given a D3D device, so this path is
/// a fallback: a fresh texture per picture keeps it simple and race-free
/// (the renderer may still hold the previous one).
pub(super) fn allocate_output_sample(
    device: &ID3D11Device,
    coded: (u32, u32),
) -> Result<IMFSample> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: coded.0,
        Height: coded.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_DECODER.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    unsafe {
        device.CreateTexture2D(&desc, None, Some(&mut texture))?;
        let texture: ID3D11Texture2D = texture.context("NV12 output texture")?;
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &texture, 0, false)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        Ok(sample)
    }
}

/// Turns a decoded sample into a [`GpuFrame`] that keeps the sample (and so
/// the decoder's texture slice) alive.
pub(super) fn to_gpu_frame(
    sample: IMFSample,
    display: (u32, u32),
    fallback_timestamp: i64,
) -> Result<GpuFrame> {
    let (texture, subresource) = unsafe {
        let buffer = sample.GetBufferByIndex(0)?;
        let dxgi: IMFDXGIBuffer = buffer
            .cast()
            .context("decoder output is in system memory, not a D3D11 texture")?;
        let mut raw = std::ptr::null_mut();
        dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw)?;
        // SAFETY: GetResource returned an AddRef'd ID3D11Texture2D.
        (ID3D11Texture2D::from_raw(raw), dxgi.GetSubresourceIndex()?)
    };
    let timestamp = unsafe { sample.GetSampleTime() }.unwrap_or(fallback_timestamp);
    Ok(GpuFrame {
        texture,
        subresource,
        width: display.0,
        height: display.1,
        timestamp,
        keepalive: Some(sample.into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mf::MediaFoundation;

    fn buffer_address(sample: &IMFSample) -> *mut std::ffi::c_void {
        unsafe { sample.GetBufferByIndex(0) }.unwrap().as_raw()
    }

    #[test]
    fn buffer_is_reused_once_the_previous_sample_is_gone() {
        let _mf = MediaFoundation::start().unwrap();
        let mut input = InputSamples::default();
        let first = input.sample(&[1, 2, 3], 10).unwrap();
        let address = buffer_address(&first);
        drop(first);
        let second = input.sample(&[4, 5, 6, 7], 20).unwrap();
        assert_eq!(buffer_address(&second), address, "same buffer");
        assert_eq!(unsafe { second.GetSampleTime() }.unwrap(), 20);
        assert_eq!(
            unsafe { second.GetBufferByIndex(0).unwrap().GetCurrentLength() }.unwrap(),
            4
        );
    }

    #[test]
    fn buffer_still_held_by_a_sample_is_not_overwritten() {
        let _mf = MediaFoundation::start().unwrap();
        let mut input = InputSamples::default();
        let held = input.sample(&[1, 2, 3], 0).unwrap();
        let next = input.sample(&[9, 9], 1).unwrap();
        assert_ne!(buffer_address(&held), buffer_address(&next));
        let mut ptr = std::ptr::null_mut();
        let buffer = unsafe { held.GetBufferByIndex(0) }.unwrap();
        unsafe { buffer.Lock(&mut ptr, None, None) }.unwrap();
        // SAFETY: Lock returned a pointer to at least 3 initialised bytes.
        assert_eq!(unsafe { std::slice::from_raw_parts(ptr, 3) }, [1, 2, 3]);
        unsafe { buffer.Unlock() }.unwrap();
    }

    #[test]
    fn buffer_grows_for_a_bigger_unit() {
        let _mf = MediaFoundation::start().unwrap();
        let mut input = InputSamples::default();
        drop(input.sample(&[0; 10], 0).unwrap());
        let big = vec![7u8; INPUT_BUFFER_STEP as usize + 1];
        let sample = input.sample(&big, 1).unwrap();
        let buffer = unsafe { sample.GetBufferByIndex(0) }.unwrap();
        assert!(unsafe { buffer.GetMaxLength() }.unwrap() > INPUT_BUFFER_STEP);
        assert_eq!(
            unsafe { buffer.GetCurrentLength() }.unwrap(),
            big.len() as u32
        );
    }
}
