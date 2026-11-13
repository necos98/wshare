//! Flip-model swap chain on an existing window.

use anyhow::{Context, Result};
use gpu::Gpu;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_FEATURE_PRESENT_ALLOW_TEARING, DXGI_MWA_NO_ALT_ENTER, DXGI_PRESENT,
    DXGI_PRESENT_ALLOW_TEARING, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIDevice1, IDXGIFactory2, IDXGIFactory5,
    IDXGISwapChain1,
};
use windows::core::{BOOL, Interface};

const BUFFER_COUNT: u32 = 2;

/// BGRA flip-discard swap chain presenting without vsync.
///
/// Flip model lets DWM show our buffer without an extra copy, and with
/// tearing allowed a frame reaches the screen as soon as it is decoded
/// instead of waiting for the next vblank (up to 16 ms at 60 Hz).
pub(crate) struct SwapChain {
    swap_chain: IDXGISwapChain1,
    tearing: bool,
    size: (u32, u32),
}

impl SwapChain {
    pub(crate) fn new(gpu: &Gpu, hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        let dxgi_device: IDXGIDevice = gpu.device.cast()?;
        let factory: IDXGIFactory2 = unsafe { dxgi_device.GetAdapter()?.GetParent()? };
        let tearing = supports_tearing(&factory);
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width.max(1),
            Height: height.max(1),
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: BUFFER_COUNT,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: flags(tearing).0 as u32,
        };
        let swap_chain = unsafe {
            factory
                .CreateSwapChainForHwnd(&gpu.device, hwnd, &desc, None, None)
                .context("CreateSwapChainForHwnd")?
        };
        unsafe {
            // The app owns the window; DXGI must not toggle fullscreen on
            // Alt+Enter behind its back.
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
            // Queue at most one frame: older frames would only add latency.
            if let Ok(device1) = gpu.device.cast::<IDXGIDevice1>() {
                let _ = device1.SetMaximumFrameLatency(1);
            }
        }
        Ok(Self {
            swap_chain,
            tearing,
            size: (width, height),
        })
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        self.size
    }

    pub(crate) fn tearing(&self) -> bool {
        self.tearing
    }

    /// The buffer to draw into. In D3D11 flip model, buffer 0 always refers
    /// to the current back buffer, so views on it stay valid across presents.
    pub(crate) fn back_buffer(&self) -> Result<ID3D11Texture2D> {
        Ok(unsafe { self.swap_chain.GetBuffer(0)? })
    }

    /// Resizes the buffers. All references to the old back buffer (views
    /// included) must have been released. A zero size (minimised window)
    /// is remembered but leaves the buffers alone.
    pub(crate) fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.size = (width, height);
        if width == 0 || height == 0 {
            return Ok(());
        }
        unsafe {
            self.swap_chain
                .ResizeBuffers(
                    BUFFER_COUNT,
                    width,
                    height,
                    DXGI_FORMAT_UNKNOWN,
                    flags(self.tearing),
                )
                .context("ResizeBuffers")
        }
    }

    pub(crate) fn present(&self) -> Result<()> {
        let flags = if self.tearing {
            DXGI_PRESENT_ALLOW_TEARING
        } else {
            DXGI_PRESENT(0)
        };
        // Occluded windows return a success status code; nothing to do.
        unsafe { self.swap_chain.Present(0, flags).ok().context("Present") }
    }
}

fn flags(tearing: bool) -> DXGI_SWAP_CHAIN_FLAG {
    if tearing {
        DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING
    } else {
        DXGI_SWAP_CHAIN_FLAG(0)
    }
}

/// Variable-refresh / tearing support needs DXGI 1.5 and a capable driver.
fn supports_tearing(factory: &IDXGIFactory2) -> bool {
    let Ok(factory5) = factory.cast::<IDXGIFactory5>() else {
        return false;
    };
    let mut allowed = BOOL::default();
    let checked = unsafe {
        factory5.CheckFeatureSupport(
            DXGI_FEATURE_PRESENT_ALLOW_TEARING,
            (&mut allowed as *mut BOOL).cast(),
            size_of::<BOOL>() as u32,
        )
    };
    checked.is_ok() && allowed.as_bool()
}
