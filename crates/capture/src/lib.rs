//! Screen capture through DXGI Desktop Duplication.
//!
//! Every frame stays on the GPU: the duplicated surface is copied into a
//! texture we own, which the next stage (colour conversion + encoder) reads
//! directly. Nothing goes through system memory.
//!
//! The mouse pointer is not part of the image (Desktop Duplication never
//! draws it); its shape and visibility are tracked separately, see
//! [`DesktopDuplication::take_pointer_changes`] and the [`pointer`] module.

pub mod pointer;

use anyhow::{Context, Result, bail};
pub use gpu::Gpu;
use gpu::wide_to_string;
pub use pointer::{MAX_POINTER_SIDE, PointerChanges, PointerImage};
use pointer::{PointerTracker, RawPointerShape, ShapeKind};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_POINTER_SHAPE_INFO, IDXGIAdapter1, IDXGIFactory1,
    IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub index: usize,
    pub name: String,
    pub adapter_name: String,
    /// Top-left corner in virtual-desktop coordinates (can be negative for
    /// monitors left of/above the primary one).
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

struct OutputHandle {
    adapter: IDXGIAdapter1,
    output: IDXGIOutput1,
    info: MonitorInfo,
}

fn enumerate_outputs() -> Result<Vec<OutputHandle>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut found = Vec::new();
    for adapter_index in 0.. {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e.into()),
        };
        let adapter_name = wide_to_string(&unsafe { adapter.GetDesc1()? }.Description);
        for output_index in 0.. {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(o) => o,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            let desc = unsafe { output.GetDesc()? };
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }
            let r = desc.DesktopCoordinates;
            found.push(OutputHandle {
                adapter: adapter.clone(),
                output: output.cast()?,
                info: MonitorInfo {
                    index: found.len(),
                    name: wide_to_string(&desc.DeviceName),
                    adapter_name: adapter_name.clone(),
                    left: r.left,
                    top: r.top,
                    width: (r.right - r.left) as u32,
                    height: (r.bottom - r.top) as u32,
                },
            });
        }
    }
    Ok(found)
}

pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
    Ok(enumerate_outputs()?.into_iter().map(|o| o.info).collect())
}

pub enum Acquire {
    /// A new desktop image was copied into [`DesktopDuplication::texture`].
    NewFrame,
    /// Nothing on screen changed (or only the mouse pointer moved).
    NoChange,
}

pub struct DesktopDuplication {
    gpu: Gpu,
    output: IDXGIOutput1,
    dup: IDXGIOutputDuplication,
    frame: ID3D11Texture2D,
    pointer: PointerTracker,
    /// Reused across `GetFramePointerShape` calls (shapes change often).
    pointer_buffer: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl DesktopDuplication {
    /// Opens the monitor with the given index and a device on its adapter.
    pub fn open(monitor_index: usize) -> Result<Self> {
        let handle = enumerate_outputs()?
            .into_iter()
            .nth(monitor_index)
            .with_context(|| format!("monitor {monitor_index} not found"))?;
        let gpu = Gpu::on_adapter(&handle.adapter)?;
        let dup = unsafe { handle.output.DuplicateOutput(&gpu.device) }
            .context("DuplicateOutput (already in use by another app?)")?;
        let mode = unsafe { dup.GetDesc() }.ModeDesc;
        let frame = create_texture(&gpu.device, mode.Width, mode.Height, mode.Format)?;
        Ok(Self {
            gpu,
            output: handle.output,
            dup,
            frame,
            pointer: PointerTracker::default(),
            pointer_buffer: Vec::new(),
            width: mode.Width,
            height: mode.Height,
        })
    }

    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// The texture that always holds the latest captured desktop image (BGRA).
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.frame
    }

    /// What changed about the mouse pointer since the previous call: a new
    /// shape (deduplicated by [`PointerImage::id`]) and/or a new visibility
    /// (`false` also when the pointer is on another monitor). Updated by
    /// [`Self::acquire`], so call it between acquires. Consumers that need
    /// the current shape later (e.g. for a newly connected viewer) must keep
    /// the last one they received.
    pub fn take_pointer_changes(&mut self) -> PointerChanges {
        self.pointer.take_changes()
    }

    /// Waits up to `timeout_ms` for the desktop to change. Pointer updates
    /// are recorded even when only the mouse changed (which still returns
    /// [`Acquire::NoChange`]).
    pub fn acquire(&mut self, timeout_ms: u32) -> Result<Acquire> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match unsafe {
            self.dup
                .AcquireNextFrame(timeout_ms, &mut info, &mut resource)
        } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(Acquire::NoChange),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                // Mode change, UAC prompt, fullscreen app... start over.
                self.reopen()?;
                return Ok(Acquire::NoChange);
            }
            Err(e) => return Err(e).context("AcquireNextFrame"),
        }
        // Must happen before ReleaseFrame: the shape belongs to this frame.
        self.record_pointer(&info);
        let result = (|| {
            if info.LastPresentTime == 0 {
                return Ok(Acquire::NoChange);
            }
            let texture: ID3D11Texture2D = resource.context("no frame resource")?.cast()?;
            unsafe { self.gpu.context.CopyResource(&self.frame, &texture) };
            Ok(Acquire::NewFrame)
        })();
        unsafe { self.dup.ReleaseFrame()? };
        result
    }

    /// Records the pointer data of the frame currently acquired. A shape that
    /// cannot be read or converted is skipped (the previous one stays in
    /// effect): a cosmetic glitch must not stop the capture.
    fn record_pointer(&mut self, info: &DXGI_OUTDUPL_FRAME_INFO) {
        // A zero LastMouseUpdateTime means the position/visibility fields
        // were not updated by this frame.
        if info.LastMouseUpdateTime != 0 {
            self.pointer
                .observe_visibility(info.PointerPosition.Visible.as_bool());
        }
        if info.PointerShapeBufferSize == 0 {
            return;
        }
        let size = info.PointerShapeBufferSize;
        self.pointer_buffer.resize(size as usize, 0);
        let mut required = 0u32;
        let mut shape_info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        // SAFETY: the buffer is valid for writes of `size` bytes (just
        // resized) and the out-pointers reference live locals. DXGI writes at
        // most `size` bytes, failing with DXGI_ERROR_MORE_DATA otherwise.
        let fetched = unsafe {
            self.dup.GetFramePointerShape(
                size,
                self.pointer_buffer.as_mut_ptr().cast(),
                &mut required,
                &mut shape_info,
            )
        };
        if fetched.is_err() {
            return;
        }
        let Some(kind) = ShapeKind::from_dxgi(shape_info.Type) else {
            return;
        };
        let raw = RawPointerShape {
            kind,
            width: shape_info.Width,
            height: shape_info.Height,
            pitch: shape_info.Pitch,
            hotspot_x: shape_info.HotSpot.x,
            hotspot_y: shape_info.HotSpot.y,
            data: &self.pointer_buffer[..(required as usize).min(size as usize)],
        };
        if let Ok(image) = pointer::convert(&raw) {
            self.pointer.observe_shape(image);
        }
    }

    /// Recreates the duplication after DXGI_ERROR_ACCESS_LOST. The pointer
    /// tracker is kept: DXGI re-sends the current shape on the new
    /// duplication, and the tracker drops it if it is unchanged.
    fn reopen(&mut self) -> Result<()> {
        let dup = loop {
            match unsafe { self.output.DuplicateOutput(&self.gpu.device) } {
                Ok(d) => break d,
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    std::thread::sleep(std::time::Duration::from_millis(50))
                }
                Err(e) => return Err(e).context("DuplicateOutput after access lost"),
            }
        };
        let mode = unsafe { dup.GetDesc() }.ModeDesc;
        if mode.Width != self.width || mode.Height != self.height {
            bail!("resolution changed to {}x{}", mode.Width, mode.Height);
        }
        self.dup = dup;
        Ok(())
    }
}

fn create_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        // Bindable so the video processor can read it as an input view.
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
    texture.context("CreateTexture2D returned nothing")
}

#[cfg(test)]
mod live_tests {
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

    use super::*;

    /// Captures monitor 0 for about a second and checks the pointer shape
    /// looks like a real cursor. Run with
    /// `cargo test -p capture -- --ignored --nocapture`.
    #[test]
    #[ignore = "requires desktop"]
    fn live_pointer_shape() {
        let mut dup = DesktopDuplication::open(0).expect("open monitor 0");
        let mut shape = None;
        let mut visible = None;
        let mut nudged = false;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(1) {
            dup.acquire(50).expect("acquire");
            let changes = dup.take_pointer_changes();
            shape = changes.shape.or(shape);
            visible = changes.visible.or(visible);
            if shape.is_none() && !nudged && start.elapsed() > Duration::from_millis(300) {
                nudged = true;
                nudge_pointer();
            }
        }
        // Changes are reported once.
        assert!(dup.take_pointer_changes().is_empty());
        let shape = shape.expect("no pointer shape within 1 s");
        println!(
            "pointer {}x{} hotspot ({},{}) visible {visible:?} id {:016x} (nudged: {nudged})",
            shape.width, shape.height, shape.hotspot_x, shape.hotspot_y, shape.id
        );
        print_ascii(&shape);

        assert!((8..=256).contains(&shape.width) && (8..=256).contains(&shape.height));
        assert!(shape.hotspot_x < shape.width && shape.hotspot_y < shape.height);
        assert_eq!(
            shape.rgba.len(),
            usize::from(shape.width) * usize::from(shape.height) * 4
        );
        let opaque = shape.rgba.chunks_exact(4).filter(|p| p[3] == 255).count();
        let transparent = shape.rgba.chunks_exact(4).filter(|p| p[3] == 0).count();
        // A few semi-transparent pixels, for eyeballing the alpha handling
        // (see the premultiplied-alpha notes in the `pointer` module).
        let edges: Vec<&[u8]> = shape
            .rgba
            .chunks_exact(4)
            .filter(|p| (1..255).contains(&p[3]))
            .take(8)
            .collect();
        println!("opaque {opaque}, transparent {transparent}, edge samples {edges:?}");
        assert!(opaque > 10, "a cursor has some opaque pixels");
        assert!(transparent > 0, "a cursor is not a solid rectangle");
    }

    /// Moves the pointer by one pixel and back, so DXGI reports it.
    fn nudge_pointer() {
        let mut p = POINT::default();
        // SAFETY: plain Win32 calls with a valid out-pointer.
        unsafe {
            if GetCursorPos(&mut p).is_ok() {
                let _ = SetCursorPos(p.x + 1, p.y);
                std::thread::sleep(Duration::from_millis(30));
                let _ = SetCursorPos(p.x, p.y);
            }
        }
    }

    fn print_ascii(shape: &PointerImage) {
        for row in shape.rgba.chunks_exact(usize::from(shape.width) * 4) {
            let line: String = row
                .chunks_exact(4)
                .map(|p| match (p[3], p[0]) {
                    (0, _) => ' ',
                    (255, 0..=63) => '#',
                    (255, 192..) => 'o',
                    (255, _) => '+',
                    _ => '.',
                })
                .collect();
            if !line.trim().is_empty() {
                println!("|{line}|");
            }
        }
    }
}
