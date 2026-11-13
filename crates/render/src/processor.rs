//! NV12 -> BGRA conversion and scaling on the GPU's video processor.
//!
//! The fixed-function video processor converts colour, scales and
//! letterboxes in a single `VideoProcessorBlt` straight into the back
//! buffer: no shaders, no intermediate texture.

use std::mem::ManuallyDrop;

use anyhow::{Context, Result};
use gpu::{Gpu, GpuFrame};
use windows::Win32::Foundation::{RECT, TRUE};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_VIDEO_COLOR,
    D3D11_VIDEO_COLOR_0, D3D11_VIDEO_COLOR_RGBA, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Texture2D, ID3D11VideoContext1, ID3D11VideoDevice,
    ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
    DXGI_RATIONAL,
};
use windows::core::Interface;

use crate::cache::BoundedCache;
use crate::layout::{Rect, fit_rect};

/// Decoder pools are typically 8-20 slices; leave room for one switch.
const INPUT_VIEW_CAPACITY: usize = 48;

/// Input views are keyed by texture identity and slice. The value keeps the
/// texture alive so its address cannot be reused by another texture while
/// the entry exists.
type InputViews = BoundedCache<(usize, u32), (ID3D11Texture2D, ID3D11VideoProcessorInputView)>;

/// Sizes a processor instance was created for. The enumerator (and with it
/// the processor and every view) is tied to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    input: (u32, u32),
    output: (u32, u32),
}

/// One processor configuration plus the views created against it.
struct Instance {
    geometry: Geometry,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    input_views: InputViews,
    output: Option<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
}

pub(crate) struct Nv12ToBgra {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext1,
    instance: Option<Instance>,
}

impl Nv12ToBgra {
    pub(crate) fn new(gpu: &Gpu) -> Result<Self> {
        Ok(Self {
            video_device: gpu.device.cast().context("device has no video support")?,
            video_context: gpu.context.cast().context("no ID3D11VideoContext1")?,
            instance: None,
        })
    }

    /// Draws `frame` letterboxed into `target` (`target_size` pixels),
    /// painting the bars black.
    pub(crate) fn blit(
        &mut self,
        frame: &GpuFrame,
        target: &ID3D11Texture2D,
        target_size: (u32, u32),
    ) -> Result<()> {
        let geometry = Geometry {
            input: (frame.width, frame.height),
            output: target_size,
        };
        self.ensure_instance(geometry)?;
        let instance = self.instance.as_mut().expect("ensured above");
        let input_view = input_view(&self.video_device, instance, frame)?;
        let output_view = output_view(&self.video_device, instance, target)?;

        let source = RECT {
            left: 0,
            top: 0,
            right: frame.width as i32,
            bottom: frame.height as i32,
        };
        let dest = to_win32(fit_rect(geometry.input, target_size));
        let processor = &instance.processor;
        let ctx = &self.video_context;
        unsafe {
            // The source rect crops the decoder's block padding (1088 -> 1080).
            ctx.VideoProcessorSetStreamSourceRect(processor, 0, true, Some(&source));
            ctx.VideoProcessorSetStreamDestRect(processor, 0, true, Some(&dest));
        }
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: TRUE,
            pInputSurface: ManuallyDrop::new(Some(input_view)),
            ..Default::default()
        };
        let result = unsafe {
            ctx.VideoProcessorBlt(processor, &output_view, 0, std::slice::from_ref(&stream))
        };
        // SAFETY: releases the reference we put in the stream description.
        unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        result.context("VideoProcessorBlt")
    }

    /// Drops every reference to the current render target (needed before
    /// the swap chain's buffers can be resized).
    pub(crate) fn release_target(&mut self) {
        if let Some(instance) = &mut self.instance {
            instance.output = None;
        }
    }

    fn ensure_instance(&mut self, geometry: Geometry) -> Result<()> {
        if self
            .instance
            .as_ref()
            .is_none_or(|i| i.geometry != geometry)
        {
            // Views belong to the old enumerator; drop them all with it.
            self.instance = None;
            self.instance = Some(self.create_instance(geometry)?);
        }
        Ok(())
    }

    fn create_instance(&self, geometry: Geometry) -> Result<Instance> {
        let rate = DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: geometry.input.0,
            InputHeight: geometry.input.1,
            OutputFrameRate: rate,
            OutputWidth: geometry.output.0,
            OutputHeight: geometry.output.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let ctx = &self.video_context;
        unsafe {
            let enumerator = self.video_device.CreateVideoProcessorEnumerator(&content)?;
            let processor = self.video_device.CreateVideoProcessor(&enumerator, 0)?;
            // Screen content: no denoise/edge enhancement guesses.
            ctx.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            ctx.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            // What the host's encoder produces: BT.709, limited range...
            ctx.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
            // ...shown as full-range sRGB-ish RGB, like the host's desktop.
            ctx.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            // Everything outside the destination rect becomes black bars.
            ctx.VideoProcessorSetOutputTargetRect(&processor, false, None);
            ctx.VideoProcessorSetOutputBackgroundColor(&processor, false, &black());
            Ok(Instance {
                geometry,
                enumerator,
                processor,
                input_views: BoundedCache::new(INPUT_VIEW_CAPACITY),
                output: None,
            })
        }
    }
}

fn input_view(
    device: &ID3D11VideoDevice,
    instance: &mut Instance,
    frame: &GpuFrame,
) -> Result<ID3D11VideoProcessorInputView> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { frame.texture.GetDesc(&mut desc) };
    let slice = frame.array_slice(desc.MipLevels);
    let key = (frame.texture.as_raw() as usize, slice);
    let enumerator = &instance.enumerator;
    let (_, view) = instance
        .input_views
        .get_or_try_insert(key, || -> Result<_> {
            let view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: slice,
                    },
                },
            };
            let mut view = None;
            unsafe {
                device
                    .CreateVideoProcessorInputView(
                        &frame.texture,
                        enumerator,
                        &view_desc,
                        Some(&mut view),
                    )
                    .context("CreateVideoProcessorInputView")?;
            }
            Ok((frame.texture.clone(), view.context("no input view")?))
        })?;
    Ok(view)
}

fn output_view(
    device: &ID3D11VideoDevice,
    instance: &mut Instance,
    target: &ID3D11Texture2D,
) -> Result<ID3D11VideoProcessorOutputView> {
    if let Some((texture, view)) = &instance.output
        && texture == target
    {
        return Ok(view.clone());
    }
    let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
        },
    };
    let mut view = None;
    unsafe {
        device
            .CreateVideoProcessorOutputView(
                target,
                &instance.enumerator,
                &view_desc,
                Some(&mut view),
            )
            .context("CreateVideoProcessorOutputView")?;
    }
    let view = view.context("no output view")?;
    instance.output = Some((target.clone(), view.clone()));
    Ok(view)
}

fn black() -> D3D11_VIDEO_COLOR {
    D3D11_VIDEO_COLOR {
        Anonymous: D3D11_VIDEO_COLOR_0 {
            RGBA: D3D11_VIDEO_COLOR_RGBA {
                R: 0.0,
                G: 0.0,
                B: 0.0,
                A: 1.0,
            },
        },
    }
}

fn to_win32(r: Rect) -> RECT {
    RECT {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}
