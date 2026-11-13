//! Handing our D3D11 device to an MFT, so it reads/writes our textures
//! directly instead of copying through system memory.

use anyhow::{Context, Result};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Media::MediaFoundation::{
    IMFDXGIDeviceManager, IMFTransform, MFCreateDXGIDeviceManager, MFT_MESSAGE_SET_D3D_MANAGER,
};
use windows::core::Interface;

/// Wraps `device` in a DXGI device manager and gives it to `transform`.
/// The returned manager must stay alive as long as the transform.
pub(crate) fn attach_d3d11_device(
    transform: &IMFTransform,
    device: &ID3D11Device,
) -> Result<IMFDXGIDeviceManager> {
    let mut reset_token = 0u32;
    let mut manager = None;
    unsafe { MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)? };
    let manager = manager.context("no DXGI device manager")?;
    unsafe {
        manager.ResetDevice(device, reset_token)?;
        transform
            .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
            .context("MFT refused our D3D11 device")?;
    }
    Ok(manager)
}
