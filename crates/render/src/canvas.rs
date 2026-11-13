//! Shared Direct2D drawing context for everything painted on top of the
//! video (statistics overlay, control bar).
//!
//! Ordering: the Direct2D device is created on the same D3D11 device, so its
//! commands go into the same immediate context as the video processor's
//! `VideoProcessorBlt` and execute after it on the GPU. `EndDraw` flushes
//! Direct2D's batch into that context before the caller presents. The whole
//! `BeginDraw`..`EndDraw` runs under the device's D3D11 multithread lock:
//! Direct2D swaps its own pipeline state into the shared immediate context,
//! and decoder threads must not slip their calls in between.

use anyhow::{Context, Result};
use gpu::Gpu;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_IGNORE, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Bitmap1, ID2D1DeviceContext,
    ID2D1Factory1,
};
use windows::Win32::Graphics::Direct3D11::{ID3D11Multithread, ID3D11Texture2D};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGISurface};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::core::Interface;

/// Device-level Direct2D/DirectWrite objects plus the target bitmap
/// wrapping the current back buffer. Painters borrow it for one
/// [`Canvas::paint`] at a time.
///
/// The context runs at 96 DPI: one DIP is one back-buffer pixel; painters
/// scale their sizes explicitly (see [`Canvas::dpi`]).
pub(crate) struct Canvas {
    hwnd: HWND,
    lock: ID3D11Multithread,
    dwrite: IDWriteFactory,
    context: ID2D1DeviceContext,
    /// Target bitmap wrapping a back buffer, with the texture it wraps.
    target: Option<(ID3D11Texture2D, ID2D1Bitmap1)>,
}

impl Canvas {
    pub(crate) fn new(gpu: &Gpu, hwnd: HWND) -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                .context("D2D1CreateFactory")?;
            let dxgi_device: IDXGIDevice = gpu.device.cast()?;
            let device = factory
                .CreateDevice(&dxgi_device)
                .context("ID2D1Factory1::CreateDevice")?;
            let context = device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            context.SetDpi(96.0, 96.0);
            context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(Self {
                hwnd,
                lock: gpu.context.cast().context("no ID3D11Multithread")?,
                dwrite: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
                    .context("DWriteCreateFactory")?,
                context,
                target: None,
            })
        }
    }

    pub(crate) fn dwrite(&self) -> &IDWriteFactory {
        &self.dwrite
    }

    /// The context, for creating device-dependent resources (brushes).
    pub(crate) fn context(&self) -> &ID2D1DeviceContext {
        &self.context
    }

    /// The window's DPI (96 = 100%).
    pub(crate) fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }
    }

    /// Runs `draw` between `BeginDraw` and `EndDraw` targeting `back_buffer`,
    /// on top of whatever the immediate context has drawn there so far.
    pub(crate) fn paint(
        &mut self,
        back_buffer: &ID3D11Texture2D,
        draw: impl FnOnce(&ID2D1DeviceContext),
    ) -> Result<()> {
        let target = self.target(back_buffer)?;
        let ctx = &self.context;
        let _guard = DeviceLock::enter(&self.lock);
        unsafe {
            ctx.SetTarget(&target);
            ctx.BeginDraw();
            draw(ctx);
            ctx.EndDraw(None, None)
                .context("ID2D1DeviceContext::EndDraw")
        }
    }

    /// Drops every reference to the back buffer (needed before the swap
    /// chain's buffers can be resized).
    pub(crate) fn release_target(&mut self) {
        if self.target.take().is_some() {
            unsafe { self.context.SetTarget(None) };
        }
    }

    fn target(&mut self, back_buffer: &ID3D11Texture2D) -> Result<ID2D1Bitmap1> {
        if let Some((texture, bitmap)) = &self.target
            && texture == back_buffer
        {
            return Ok(bitmap.clone());
        }
        self.release_target();
        let surface: IDXGISurface = back_buffer.cast()?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            ..Default::default()
        };
        let bitmap = unsafe {
            self.context
                .CreateBitmapFromDxgiSurface(&surface, Some(&props))
                .context("CreateBitmapFromDxgiSurface")?
        };
        self.target = Some((back_buffer.clone(), bitmap.clone()));
        Ok(bitmap)
    }
}

/// Holds the D3D11 device's (recursive) multithread lock while alive.
struct DeviceLock<'a>(&'a ID3D11Multithread);

impl<'a> DeviceLock<'a> {
    fn enter(lock: &'a ID3D11Multithread) -> Self {
        unsafe { lock.Enter() };
        Self(lock)
    }
}

impl Drop for DeviceLock<'_> {
    fn drop(&mut self) {
        unsafe { self.0.Leave() };
    }
}
