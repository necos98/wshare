//! D3D11 device creation and the GPU frame type shared by capture, codec and
//! render.

mod frame;

pub use frame::{GpuFrame, array_slice};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1};
use windows::core::Interface;

/// A D3D11 device (with video support) and the adapter it lives on.
#[derive(Clone)]
pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub adapter_luid: LUID,
    pub adapter_name: String,
}

impl Gpu {
    /// Creates a device on a specific adapter (e.g. the one driving a monitor).
    pub fn on_adapter(adapter: &IDXGIAdapter1) -> Result<Self> {
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .context("D3D11CreateDevice")?;
        }
        let device: ID3D11Device = device.context("no D3D11 device")?;
        let context = context.context("no D3D11 context")?;
        // Media Foundation uses the device from its own worker threads.
        unsafe {
            let _ = device
                .cast::<ID3D11Multithread>()?
                .SetMultithreadProtected(true);
        }
        let desc = unsafe { adapter.GetDesc1()? };
        Ok(Self {
            device,
            context,
            adapter_luid: desc.AdapterLuid,
            adapter_name: wide_to_string(&desc.Description),
        })
    }

    /// Creates a device on the system's default (first) adapter.
    pub fn default_adapter() -> Result<Self> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
        let adapter = unsafe { factory.EnumAdapters1(0)? };
        Self::on_adapter(&adapter)
    }
}

/// Converts a NUL-terminated UTF-16 buffer from a Win32 struct.
pub fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_to_string_stops_at_nul() {
        let wide: Vec<u16> = "GPU\0garbage".encode_utf16().collect();
        assert_eq!(wide_to_string(&wide), "GPU");
    }

    #[test]
    fn wide_to_string_without_nul_uses_whole_buffer() {
        let wide: Vec<u16> = "NVIDIA".encode_utf16().collect();
        assert_eq!(wide_to_string(&wide), "NVIDIA");
    }
}
