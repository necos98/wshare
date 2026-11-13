//! The viewer's frame sink: decoded NV12 frames -> window.

use anyhow::Result;
use gpu::{Gpu, GpuFrame};
use pipeline::FrameSink;
use windows::Win32::Foundation::HWND;

use crate::processor::Nv12ToBgra;
use crate::swapchain::SwapChain;

/// Shows [`GpuFrame`]s in a window created by someone else (the app).
///
/// Composes a [`SwapChain`] (where pixels go) with the video processor
/// (how NV12 becomes letterboxed BGRA); it only sequences the two.
pub struct Renderer {
    swap_chain: SwapChain,
    processor: Nv12ToBgra,
}

impl Renderer {
    /// Attaches to `hwnd`, whose client area is `width` x `height` pixels.
    /// `gpu` must be the device the frames are decoded on.
    pub fn new(gpu: &Gpu, hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        Ok(Self {
            swap_chain: SwapChain::new(gpu, hwnd, width, height)?,
            processor: Nv12ToBgra::new(gpu)?,
        })
    }

    /// Call when the window's client area changed size. Zero sizes
    /// (minimised) are allowed; frames are then skipped.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if self.swap_chain.size() == (width, height) {
            return Ok(());
        }
        self.processor.release_target();
        self.swap_chain.resize(width, height)
    }

    /// True when frames are presented immediately (tearing allowed) rather
    /// than on the next vblank.
    pub fn tearing(&self) -> bool {
        self.swap_chain.tearing()
    }

    /// Draws `frame` into the back buffer. Returns false when there is
    /// nothing to draw into (minimised window).
    fn draw(&mut self, frame: &GpuFrame) -> Result<bool> {
        let size = self.swap_chain.size();
        if size.0 == 0 || size.1 == 0 || frame.width == 0 || frame.height == 0 {
            return Ok(false);
        }
        let back_buffer = self.swap_chain.back_buffer()?;
        self.processor.blit(frame, &back_buffer, size)?;
        Ok(true)
    }
}

impl FrameSink for Renderer {
    type Frame = GpuFrame;

    fn present(&mut self, frame: &GpuFrame) -> Result<()> {
        if self.draw(frame)? {
            self.swap_chain.present()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use anyhow::Context;
    use codec::{
        BgraToNv12, Codec, EncoderConfig, HwDecoder, HwEncoder, MediaFoundation, VideoDecoder,
        VideoEncoder,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_DECODER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
        D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SUBRESOURCE_DATA,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC,
    };

    use super::*;
    use crate::test_window::HiddenWindow;

    const RED: [u8; 3] = [200, 40, 40];
    const GREEN: [u8; 3] = [40, 200, 40];

    #[test]
    #[ignore = "requires GPU"]
    fn draws_slice_letterboxed_with_bt709_conversion() -> Result<()> {
        let gpu = Gpu::default_adapter()?;
        let window = HiddenWindow::new(800, 600)?;
        let mut renderer = Renderer::new(&gpu, window.hwnd(), 800, 600)?;
        // 640x360 picture in slice 1 of a 640x368 array, like a decoder's
        // padded output; slice 0 holds another colour.
        let texture = nv12_array(&gpu, (640, 368), 360, &[GREEN, RED])?;
        let frame = GpuFrame {
            texture,
            subresource: 1,
            width: 640,
            height: 360,
            timestamp: 0,
            keepalive: None,
        };

        assert!(renderer.draw(&frame)?);
        // 16:9 in 800x600 -> 800x450 at y = 75..525.
        let pixels = read_back_buffer(
            &gpu,
            &renderer,
            &[(400, 10), (400, 300), (400, 523), (400, 590)],
        )?;
        assert_eq!(pixels[0], [0, 0, 0], "top bar");
        assert_close(pixels[1], RED, "centre");
        assert_close(pixels[2], RED, "last picture row (padding must be cropped)");
        assert_eq!(pixels[3], [0, 0, 0], "bottom bar");
        renderer.present(&frame)?;

        renderer.resize(300, 500)?;
        renderer.present(&frame)?;
        renderer.resize(0, 0)?;
        renderer.present(&frame)?;
        Ok(())
    }

    #[test]
    #[ignore = "requires GPU"]
    fn presents_decoded_frames() -> Result<()> {
        let _mf = MediaFoundation::start()?;
        let gpu = Gpu::default_adapter()?;
        let (width, height) = (1280, 720);
        let window = HiddenWindow::new(960, 540)?;
        let mut renderer = Renderer::new(&gpu, window.hwnd(), 960, 540)?;

        let source = bgra_texture(&gpu, width, height)?;
        let mut converter = BgraToNv12::new(&gpu, &source, width, height, 60)?;
        let config = EncoderConfig {
            codec: Codec::H264,
            width,
            height,
            fps: 60,
            bitrate_bps: 10_000_000,
        };
        let mut encoder = HwEncoder::new(&gpu.device, gpu.adapter_luid, &config)?;
        let mut decoder = HwDecoder::new(&gpu, Codec::H264, width, height)?;

        let mut encoded = VecDeque::new();
        for (index, [r, g, b]) in [RED, GREEN, RED, GREEN].into_iter().enumerate() {
            let pixels = [b, g, r, 255].repeat((width * height) as usize);
            unsafe {
                gpu.context.UpdateSubresource(
                    &source,
                    0,
                    None,
                    pixels.as_ptr().cast(),
                    width * 4,
                    0,
                );
            }
            while !encoder.can_accept() {
                encoder.poll(true, &mut encoded)?;
            }
            encoder.submit(&converter.convert()?, index as i64)?;
            while encoded.is_empty() {
                encoder.poll(true, &mut encoded)?;
            }
            let unit = encoded.pop_front().context("no unit")?;
            let frame = decoder
                .decode(&unit.data, unit.timestamp)?
                .context("no picture")?;
            assert!(renderer.draw(&frame)?);
            let [centre] = read_back_buffer(&gpu, &renderer, &[(480, 270)])?[..] else {
                unreachable!()
            };
            assert_close(centre, [r, g, b], "decoded centre");
            renderer.present(&frame)?;
        }
        Ok(())
    }

    fn assert_close(actual: [u8; 3], expected: [u8; 3], what: &str) {
        let off = actual.iter().zip(expected).any(|(a, e)| a.abs_diff(e) > 12);
        assert!(!off, "{what}: got {actual:?}, expected {expected:?}");
    }

    /// RGB -> BT.709 limited-range YCbCr.
    fn to_ycbcr([r, g, b]: [u8; 3]) -> [u8; 3] {
        let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let cb = (b - y) / 1.8556;
        let cr = (r - y) / 1.5748;
        let q = |v: f32| v.round().clamp(0.0, 255.0) as u8;
        [
            q(16.0 + 219.0 * y),
            q(128.0 + 224.0 * cb),
            q(128.0 + 224.0 * cr),
        ]
    }

    /// An NV12 texture array with one solid colour per slice. Rows from
    /// `visible_height` down are filled with white, to catch padding leaks.
    fn nv12_array(
        gpu: &Gpu,
        (width, height): (u32, u32),
        visible_height: u32,
        colours: &[[u8; 3]],
    ) -> Result<ID3D11Texture2D> {
        let planes: Vec<Vec<u8>> = colours
            .iter()
            .map(|&colour| {
                let [y, cb, cr] = to_ycbcr(colour);
                let mut data = Vec::new();
                for row in 0..height {
                    let luma = if row < visible_height { y } else { 235 };
                    data.extend(std::iter::repeat_n(luma, width as usize));
                }
                for row in 0..height / 2 {
                    let (u, v) = if row * 2 < visible_height {
                        (cb, cr)
                    } else {
                        (128, 128)
                    };
                    data.extend([u, v].repeat(width as usize / 2));
                }
                data
            })
            .collect();
        let init: Vec<D3D11_SUBRESOURCE_DATA> = planes
            .iter()
            .map(|p| D3D11_SUBRESOURCE_DATA {
                pSysMem: p.as_ptr().cast(),
                SysMemPitch: width,
                SysMemSlicePitch: 0,
            })
            .collect();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: colours.len() as u32,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_DECODER.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe {
            gpu.device
                .CreateTexture2D(&desc, Some(init.as_ptr()), Some(&mut texture))?
        };
        texture.context("NV12 array")
    }

    fn bgra_texture(gpu: &Gpu, width: u32, height: u32) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe {
            gpu.device
                .CreateTexture2D(&desc, None, Some(&mut texture))?
        };
        texture.context("BGRA texture")
    }

    /// Reads RGB pixels of the current back buffer (before presenting).
    fn read_back_buffer(
        gpu: &Gpu,
        renderer: &Renderer,
        points: &[(usize, usize)],
    ) -> Result<Vec<[u8; 3]>> {
        let back_buffer = renderer.swap_chain.back_buffer()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { back_buffer.GetDesc(&mut desc) };
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut staging = None;
        unsafe {
            gpu.device
                .CreateTexture2D(&desc, None, Some(&mut staging))?
        };
        let staging: ID3D11Texture2D = staging.context("staging")?;
        unsafe {
            gpu.context.CopyResource(&staging, &back_buffer);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            gpu.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let data = mapped.pData as *const u8;
            let pixels = points
                .iter()
                .map(|&(x, y)| {
                    let p = data.add(y * mapped.RowPitch as usize + x * 4);
                    [*p.add(2), *p.add(1), *p]
                })
                .collect();
            gpu.context.Unmap(&staging, 0);
            Ok(pixels)
        }
    }
}
