//! Hardware-accelerated H.264 / HEVC decoding through Media Foundation.
//!
//! We use the system's synchronous decoder MFTs (Microsoft's H.264/HEVC
//! decoders), which run on the GPU through DXVA once they are given our
//! D3D11 device, and write each picture into a slice of a texture array the
//! renderer can read without any copy.

mod format;
mod samples;

use anyhow::{Context, Result, bail};
use gpu::{Gpu, GpuFrame};
use pipeline::VideoDecoder;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Media::MediaFoundation::*;
use windows::core::Interface;

use self::format::OutputFormat;
use crate::Codec;
use crate::mf;

/// Synchronous, low-latency hardware decoder producing NV12 [`GpuFrame`]s.
///
/// Expects Annex-B access units without B-frames (as produced by
/// [`crate::HwEncoder`]); each unit then yields its picture immediately.
pub struct HwDecoder {
    transform: IMFTransform,
    _device_manager: IMFDXGIDeviceManager,
    device: ID3D11Device,
    /// The size announced by the caller, used to crop the coded size when
    /// the decoder does not report a display aperture.
    requested: (u32, u32),
    /// `None` until the decoder has parsed the stream's parameter sets.
    format: Option<OutputFormat>,
    provides_samples: bool,
    input: samples::InputSamples,
    name: String,
}

impl HwDecoder {
    /// Creates a decoder for `codec` on `gpu`'s device. `width`/`height` are
    /// the expected visible size; the real one is taken from the stream.
    pub fn new(gpu: &Gpu, codec: Codec, width: u32, height: u32) -> Result<Self> {
        let (transform, name) = find_d3d11_decoder(codec)?;
        let device_manager = mf::attach_d3d11_device(&transform, &gpu.device)?;
        enable_low_latency(&transform);
        format::set_input_type(&transform, codec, (width, height))?;

        let mut decoder = Self {
            transform,
            _device_manager: device_manager,
            device: gpu.device.clone(),
            requested: (width, height),
            format: None,
            provides_samples: true,
            input: samples::InputSamples::default(),
            name,
        };
        // Most decoders accept NV12 right away; others only after parsing
        // the SPS, which they signal with a stream change.
        if decoder.select_output().is_err() {
            decoder.format = None;
        }
        unsafe {
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(decoder)
    }

    /// The MFT's friendly name, e.g. "Microsoft H264 Video Decoder MFT".
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Visible size of the pictures, once the stream has been parsed.
    pub fn display_size(&self) -> Option<(u32, u32)> {
        self.format.map(|f| f.display)
    }

    /// (Re)selects NV12 output and refreshes everything derived from it.
    fn select_output(&mut self) -> Result<()> {
        let format = format::select_nv12_output(&self.transform, self.requested)?;
        let info = unsafe { self.transform.GetOutputStreamInfo(0)? };
        let provide_flags =
            (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
        self.provides_samples = info.dwFlags & provide_flags != 0;
        self.format = Some(format);
        Ok(())
    }

    /// Pulls every picture the decoder has ready and returns the newest one
    /// (older ones would be stale by the time they were shown).
    fn drain_output(&mut self, timestamp: i64) -> Result<Option<GpuFrame>> {
        let mut latest = None;
        loop {
            let provided = match self.format {
                Some(format) if !self.provides_samples => {
                    Some(samples::allocate_output_sample(&self.device, format.coded)?)
                }
                _ => None,
            };
            let (result, sample) = mf::process_output(&self.transform, provided);
            match result {
                Ok(()) => {
                    let display = self.format.context("picture before output type")?.display;
                    if let Some(sample) = sample {
                        latest = Some(samples::to_gpu_frame(sample, display, timestamp)?);
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(latest),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // New SPS (first keyframe, resolution change): the output
                    // type was reset and must be negotiated again.
                    self.select_output()
                        .context("renegotiating decoder output")?;
                }
                Err(e) => return Err(e).context("decoder ProcessOutput"),
            }
        }
    }
}

impl VideoDecoder for HwDecoder {
    type Frame = GpuFrame;

    fn decode(&mut self, unit: &[u8], timestamp: i64) -> Result<Option<GpuFrame>> {
        let sample = self.input.sample(unit, timestamp)?;
        let mut latest = None;
        loop {
            match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
                Ok(()) => break,
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    // Output is pending; collect it, then retry the input.
                    match self.drain_output(timestamp)? {
                        Some(frame) => latest = Some(frame),
                        None => bail!("decoder refuses input but has no output"),
                    }
                }
                Err(e) => return Err(e).context("decoder ProcessInput"),
            }
        }
        Ok(self.drain_output(timestamp)?.or(latest))
    }

    fn reset(&mut self) -> Result<()> {
        // Drops the queued units and pictures and the references of the old
        // stream; the parsed output format stays, a new SPS renegotiates it.
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)
                .context("flushing the decoder")?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .context("restarting the decoder stream")?;
        }
        Ok(())
    }
}

impl Drop for HwDecoder {
    fn drop(&mut self) {
        mf::shutdown(&self.transform);
    }
}

/// Finds a synchronous decoder MFT that can decode into D3D11 textures.
/// Decoders that are not D3D11-aware would decode on the CPU into system
/// memory, which neither meets our latency budget nor feeds the renderer.
fn find_d3d11_decoder(codec: Codec) -> Result<(IMFTransform, String)> {
    let candidates = mf::enumerate_transforms(
        MFT_CATEGORY_VIDEO_DECODER,
        MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
        codec.subtype(),
        MFVideoFormat_NV12,
    )?;
    for activate in candidates {
        let Ok(transform) = (unsafe { activate.ActivateObject::<IMFTransform>() }) else {
            continue;
        };
        let d3d11_aware = unsafe { transform.GetAttributes() }
            .and_then(|a| unsafe { a.GetUINT32(&MF_SA_D3D11_AWARE) })
            .unwrap_or(0)
            != 0;
        if d3d11_aware {
            return Ok((transform, mf::friendly_name(&activate)));
        }
        unsafe {
            let _ = activate.ShutdownObject();
        }
    }
    bail!("no D3D11-accelerated {codec:?} decoder found (HEVC needs the HEVC Video Extensions)")
}

/// Without these the decoder holds pictures back to reorder frames, adding
/// at least one frame of latency even for streams without B-frames.
fn enable_low_latency(transform: &IMFTransform) {
    unsafe {
        if let Ok(attributes) = transform.GetAttributes() {
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
        }
        if let Ok(api) = transform.cast::<ICodecAPI>() {
            let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &mf::variant_bool(true));
        }
    }
}
