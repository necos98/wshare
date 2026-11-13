//! The viewer's frame sink: decoded NV12 frames -> window.

use anyhow::Result;
use gpu::{Gpu, GpuFrame};
use pipeline::FrameSink;
use windows::Win32::Foundation::HWND;

use crate::canvas::Canvas;
use crate::overlay::Overlay;
use crate::processor::Nv12ToBgra;
use crate::swapchain::SwapChain;
use crate::toolbar_painter::ToolbarPainter;

/// Shows [`GpuFrame`]s in a window created by someone else (the app).
///
/// Composes a [`SwapChain`] (where pixels go), the video processor (how
/// NV12 becomes letterboxed BGRA) and an optional text [`Overlay`] drawn on
/// top; it only sequences them.
pub struct Renderer {
    swap_chain: SwapChain,
    processor: Nv12ToBgra,
    canvas: Canvas,
    overlay: Overlay,
    toolbar: ToolbarPainter,
}

impl Renderer {
    /// Attaches to `hwnd`, whose client area is `width` x `height` pixels.
    /// `gpu` must be the device the frames are decoded on.
    pub fn new(gpu: &Gpu, hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        let canvas = Canvas::new(gpu, hwnd)?;
        Ok(Self {
            swap_chain: SwapChain::new(gpu, hwnd, width, height)?,
            processor: Nv12ToBgra::new(gpu)?,
            overlay: Overlay::new(&canvas)?,
            toolbar: ToolbarPainter::new(&canvas)?,
            canvas,
        })
    }

    /// Shows `lines` of text in the top-left corner on every subsequent
    /// [`FrameSink::present`]; `None` hides it. Takes effect on the next
    /// present (re-present the last frame to refresh a static picture).
    pub fn set_overlay(&mut self, lines: Option<Vec<String>>) {
        self.overlay.set_lines(lines);
    }

    /// Shows (Some) or hides the full-screen control bar; drawn on every
    /// subsequent present until changed, on top of video and statistics.
    pub fn set_toolbar(&mut self, view: Option<crate::ToolbarView>) {
        self.toolbar.set_view(view);
    }

    /// Call when the window's client area changed size. Zero sizes
    /// (minimised) are allowed; frames are then skipped.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if self.swap_chain.size() == (width, height) {
            return Ok(());
        }
        self.processor.release_target();
        self.canvas.release_target();
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
        // Same immediate context: the overlay's Direct2D commands land
        // after the blit, and EndDraw flushes them before the present.
        self.processor.blit(frame, &back_buffer, size)?;
        self.overlay.draw(&mut self.canvas, &back_buffer, size)?;
        self.toolbar.draw(&mut self.canvas, &back_buffer, size)?;
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
        let image = Image::read(gpu, renderer)?;
        Ok(points.iter().map(|&(x, y)| image.at(x, y)).collect())
    }

    /// A CPU copy of the back buffer, RGB.
    #[derive(PartialEq)]
    struct Image {
        width: usize,
        height: usize,
        pixels: Vec<[u8; 3]>,
    }

    impl std::fmt::Debug for Image {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Image {}x{}", self.width, self.height)
        }
    }

    impl Image {
        fn read(gpu: &Gpu, renderer: &Renderer) -> Result<Self> {
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
            let (width, height) = (desc.Width as usize, desc.Height as usize);
            unsafe {
                gpu.context.CopyResource(&staging, &back_buffer);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                gpu.context
                    .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                let data = mapped.pData as *const u8;
                let mut pixels = Vec::with_capacity(width * height);
                for y in 0..height {
                    for x in 0..width {
                        let p = data.add(y * mapped.RowPitch as usize + x * 4);
                        pixels.push([*p.add(2), *p.add(1), *p]);
                    }
                }
                gpu.context.Unmap(&staging, 0);
                Ok(Self {
                    width,
                    height,
                    pixels,
                })
            }
        }

        fn at(&self, x: usize, y: usize) -> [u8; 3] {
            self.pixels[y * self.width + x]
        }

        /// Bounding box (left, top, right, bottom; exclusive) of the pixels
        /// that differ from `other`, if any.
        fn diff_bounds(&self, other: &Image) -> Option<Bounds> {
            let mut bounds: Option<Bounds> = None;
            for y in 0..self.height {
                for x in 0..self.width {
                    if self.at(x, y) != other.at(x, y) {
                        let b = bounds.get_or_insert((x, y, x + 1, y + 1));
                        *b = (b.0.min(x), b.1.min(y), b.2.max(x + 1), b.3.max(y + 1));
                    }
                }
            }
            bounds
        }

        /// Pixels with every channel above `threshold` inside `bounds`.
        fn bright_pixels(&self, (l, t, r, b): Bounds, threshold: u8) -> usize {
            (t..b)
                .flat_map(|y| (l..r).map(move |x| (x, y)))
                .filter(|&(x, y)| self.at(x, y).iter().all(|&c| c > threshold))
                .count()
        }
    }

    /// (left, top, right, bottom), right/bottom exclusive.
    type Bounds = (usize, usize, usize, usize);

    const STATS: [&str; 3] = ["1920x1080 60 fps", "12.3 Mbit/s  rtt 2 ms", "WWWW ####"];

    fn stats() -> Option<Vec<String>> {
        Some(STATS.iter().map(|s| s.to_string()).collect())
    }

    /// A red picture filling a 16:9 window entirely (no bars), so the
    /// overlay sits on video.
    fn red_frame(gpu: &Gpu) -> Result<GpuFrame> {
        Ok(GpuFrame {
            texture: nv12_array(gpu, (640, 368), 360, &[GREEN, RED])?,
            subresource: 1,
            width: 640,
            height: 360,
            timestamp: 0,
            keepalive: None,
        })
    }

    /// Checks the overlay in `with` against the same frame `without` it:
    /// a dark box in the top-left corner with white text, nothing else
    /// touched.
    fn assert_overlay(with: &Image, without: &Image) {
        let bounds = with.diff_bounds(without).expect("overlay changed nothing");
        let (l, t, r, b) = bounds;
        assert!(l >= 4 && t >= 4, "box keeps a margin: {bounds:?}");
        assert!(
            r < with.width / 2 && b < with.height / 2,
            "box stays small and top-left: {bounds:?}"
        );
        // Inside the box, left of the text (padding): darkened video.
        let (bx, by) = (l + 3, (t + b) / 2);
        let (dark, video) = (with.at(bx, by), without.at(bx, by));
        assert!(
            dark.iter().zip(video).all(|(&d, v)| d <= v) && dark[0] + 40 < video[0],
            "box darkens the video: {dark:?} vs {video:?}"
        );
        assert!(
            with.bright_pixels(bounds, 200) > 20,
            "white text inside the box"
        );
        assert_eq!(
            without.bright_pixels(bounds, 200),
            0,
            "red video has no white of its own"
        );
        // Outside the box nothing changed (diff_bounds proves it); spot-check.
        let (fx, fy) = (with.width - 10, with.height - 10);
        assert_eq!(with.at(fx, fy), without.at(fx, fy));
    }

    #[test]
    #[ignore = "requires GPU"]
    fn overlay_draws_dark_box_with_text_over_video() -> Result<()> {
        let gpu = Gpu::default_adapter()?;
        let window = HiddenWindow::new(640, 360)?;
        let mut renderer = Renderer::new(&gpu, window.hwnd(), 640, 360)?;
        let frame = red_frame(&gpu)?;

        assert!(renderer.draw(&frame)?);
        let plain = Image::read(&gpu, &renderer)?;
        assert_close(plain.at(10, 10), RED, "video under the future box");
        renderer.present(&frame)?;

        renderer.set_overlay(stats());
        assert!(renderer.draw(&frame)?);
        let with = Image::read(&gpu, &renderer)?;
        assert_overlay(&with, &plain);
        renderer.present(&frame)?;

        // Drawn again on every present, identically.
        assert!(renderer.draw(&frame)?);
        assert_eq!(Image::read(&gpu, &renderer)?, with, "overlay is stable");
        renderer.present(&frame)?;

        // Off: the frame is exactly what it was before the overlay.
        renderer.set_overlay(None);
        assert!(renderer.draw(&frame)?);
        assert_eq!(Image::read(&gpu, &renderer)?, plain, "overlay off");
        renderer.present(&frame)
    }

    #[test]
    #[ignore = "requires GPU"]
    fn overlay_survives_resize() -> Result<()> {
        let gpu = Gpu::default_adapter()?;
        let window = HiddenWindow::new(640, 360)?;
        let mut renderer = Renderer::new(&gpu, window.hwnd(), 640, 360)?;
        let frame = red_frame(&gpu)?;
        renderer.set_overlay(stats());
        renderer.present(&frame)?;

        for (width, height) in [(960, 540), (0, 0), (480, 270), (1280, 720)] {
            renderer.resize(width, height)?;
            renderer.present(&frame)?;
            if width == 0 {
                continue;
            }
            renderer.set_overlay(None);
            assert!(renderer.draw(&frame)?);
            let plain = Image::read(&gpu, &renderer)?;
            assert_eq!(
                (plain.width, plain.height),
                (width as usize, height as usize)
            );
            renderer.set_overlay(stats());
            assert!(renderer.draw(&frame)?);
            assert_overlay(&Image::read(&gpu, &renderer)?, &plain);
            renderer.present(&frame)?;
        }
        Ok(())
    }

    // ---- control bar ----

    use crate::layout::Rect;
    use crate::toolbar::ToolbarView;
    use crate::toolbar_geometry::{ToolbarGeometry, toolbar_geometry};

    fn bar_view(expanded: bool, hovered: Option<usize>) -> Option<ToolbarView> {
        Some(ToolbarView {
            buttons: ["Window", "Stats", "Quit"].map(String::from).to_vec(),
            expanded,
            hovered,
        })
    }

    /// The geometry the renderer will use for this window.
    fn bar_geometry(window: &HiddenWindow, size: (u32, u32), expanded: bool) -> ToolbarGeometry {
        let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(window.hwnd()) };
        toolbar_geometry(size, dpi as f32 / 96.0, 3, expanded)
    }

    fn inside(bounds: Bounds, r: &Rect) -> bool {
        bounds.0 as i32 >= r.left
            && bounds.1 as i32 >= r.top
            && bounds.2 as i32 <= r.right
            && bounds.3 as i32 <= r.bottom
    }

    fn render(gpu: &Gpu, renderer: &mut Renderer, frame: &GpuFrame) -> Result<Image> {
        assert!(renderer.draw(frame)?);
        Image::read(gpu, renderer)
    }

    fn rect_bounds(r: &Rect) -> Bounds {
        (
            r.left as usize,
            r.top as usize,
            r.right as usize,
            r.bottom as usize,
        )
    }

    #[test]
    #[ignore = "requires GPU"]
    fn expanded_toolbar_draws_bar_buttons_and_labels() -> Result<()> {
        let gpu = Gpu::default_adapter()?;
        let window = HiddenWindow::new(960, 540)?;
        let mut renderer = Renderer::new(&gpu, window.hwnd(), 960, 540)?;
        let frame = red_frame(&gpu)?;
        let plain = render(&gpu, &mut renderer, &frame)?;
        let g = bar_geometry(&window, (960, 540), true);

        renderer.set_toolbar(bar_view(true, None));
        let with = render(&gpu, &mut renderer, &frame)?;
        let bounds = with.diff_bounds(&plain).expect("bar changed nothing");
        assert!(
            inside(bounds, &g.bar),
            "only the bar changes: {bounds:?} vs {:?}",
            g.bar
        );
        // Padding pixel, high enough to dodge the rounded bottom corners.
        let (px, py) = (g.bar.left as usize + 3, 2);
        assert_ne!(with.at(px, py), plain.at(px, py), "bar fill");
        for (i, b) in g.buttons.iter().enumerate() {
            let r = rect_bounds(b);
            assert!(with.bright_pixels(r, 200) > 10, "label of button {i}");
            assert_eq!(plain.bright_pixels(r, 200), 0);
        }
        // The gap between two buttons shows no label pixels.
        let gap = Rect {
            left: g.buttons[0].right,
            right: g.buttons[1].left,
            ..g.buttons[0]
        };
        assert_eq!(with.bright_pixels(rect_bounds(&gap), 200), 0);

        // Hover highlights exactly that button.
        renderer.set_toolbar(bar_view(true, Some(1)));
        let hover = render(&gpu, &mut renderer, &frame)?;
        let b1 = &g.buttons[1];
        let (hx, hy) = (b1.left as usize + 8, (b1.top + b1.bottom) as usize / 2);
        assert_ne!(hover.at(hx, hy), with.at(hx, hy), "hovered fill differs");
        let b0 = &g.buttons[0];
        let (ox, oy) = (b0.left as usize + 8, (b0.top + b0.bottom) as usize / 2);
        assert_eq!(hover.at(ox, oy), with.at(ox, oy), "other button unchanged");
        let hb = hover.diff_bounds(&with).expect("hover differs");
        assert!(inside(hb, b1), "hover only touches its button: {hb:?}");
        renderer.present(&frame)?;

        // Off: identical to the plain frame.
        renderer.set_toolbar(None);
        assert_eq!(render(&gpu, &mut renderer, &frame)?, plain);
        renderer.present(&frame)
    }

    #[test]
    #[ignore = "requires GPU"]
    fn collapsed_toolbar_draws_only_the_handle() -> Result<()> {
        let gpu = Gpu::default_adapter()?;
        let window = HiddenWindow::new(960, 540)?;
        let mut renderer = Renderer::new(&gpu, window.hwnd(), 960, 540)?;
        let frame = red_frame(&gpu)?;
        let plain = render(&gpu, &mut renderer, &frame)?;
        let g = bar_geometry(&window, (960, 540), false);

        renderer.set_toolbar(bar_view(false, None));
        let with = render(&gpu, &mut renderer, &frame)?;
        let bounds = with.diff_bounds(&plain).expect("handle changed nothing");
        assert!(inside(bounds, &g.handle), "{bounds:?} vs {:?}", g.handle);
        let (cx, cy) = ((g.handle.left + g.handle.right) as usize / 2, 1);
        assert_ne!(with.at(cx, cy), plain.at(cx, cy));

        renderer.set_toolbar(None);
        assert_eq!(render(&gpu, &mut renderer, &frame)?, plain);
        Ok(())
    }

    #[test]
    #[ignore = "requires GPU"]
    fn toolbar_coexists_with_statistics_overlay() -> Result<()> {
        let gpu = Gpu::default_adapter()?;
        let window = HiddenWindow::new(960, 540)?;
        let mut renderer = Renderer::new(&gpu, window.hwnd(), 960, 540)?;
        let frame = red_frame(&gpu)?;
        let g = bar_geometry(&window, (960, 540), true);

        renderer.set_overlay(stats());
        let stats_only = render(&gpu, &mut renderer, &frame)?;
        renderer.set_toolbar(bar_view(true, Some(0)));
        let both = render(&gpu, &mut renderer, &frame)?;
        let bounds = both.diff_bounds(&stats_only).expect("bar missing");
        assert!(inside(bounds, &g.bar), "{bounds:?}");

        // Overlay still there with the bar on, and the bar is unaffected by it.
        renderer.set_overlay(None);
        let bar_only = render(&gpu, &mut renderer, &frame)?;
        assert_ne!(both, bar_only, "stats overlay still drawn");
        renderer.set_toolbar(None);
        let plain = render(&gpu, &mut renderer, &frame)?;
        assert_eq!(
            bar_only.diff_bounds(&plain).map(|b| inside(b, &g.bar)),
            Some(true)
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires GPU"]
    fn toolbar_survives_resize() -> Result<()> {
        let gpu = Gpu::default_adapter()?;
        let window = HiddenWindow::new(960, 540)?;
        let mut renderer = Renderer::new(&gpu, window.hwnd(), 960, 540)?;
        let frame = red_frame(&gpu)?;
        renderer.set_toolbar(bar_view(true, None));
        renderer.present(&frame)?;

        for size in [(1280, 720), (0, 0), (640, 360), (200, 120)] {
            renderer.resize(size.0, size.1)?;
            renderer.present(&frame)?;
            if size.0 == 0 {
                continue;
            }
            renderer.set_toolbar(None);
            let plain = render(&gpu, &mut renderer, &frame)?;
            assert_eq!(
                (plain.width, plain.height),
                (size.0 as usize, size.1 as usize)
            );
            renderer.set_toolbar(bar_view(true, None));
            let with = render(&gpu, &mut renderer, &frame)?;
            let g = bar_geometry(&window, size, true);
            let bounds = with.diff_bounds(&plain).expect("bar missing after resize");
            assert!(
                inside(bounds, &g.bar),
                "{size:?}: {bounds:?} vs {:?}",
                g.bar
            );
            renderer.present(&frame)?;
        }
        Ok(())
    }
}
