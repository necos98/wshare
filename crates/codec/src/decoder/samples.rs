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

/// Wraps one Annex-B access unit in a system-memory sample.
pub(super) fn compressed_sample(unit: &[u8], timestamp: i64) -> Result<IMFSample> {
    let len = u32::try_from(unit.len()).context("access unit too large")?;
    unsafe {
        let buffer = MFCreateMemoryBuffer(len)?;
        let mut ptr = std::ptr::null_mut();
        buffer.Lock(&mut ptr, None, None)?;
        // SAFETY: the buffer was created with room for `len` bytes.
        std::ptr::copy_nonoverlapping(unit.as_ptr(), ptr, unit.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(len)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(timestamp)?;
        Ok(sample)
    }
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
