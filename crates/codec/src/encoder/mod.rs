//! Hardware H.264 / HEVC encoding through Media Foundation.
//!
//! Media Foundation exposes each GPU vendor's encoder (Intel QuickSync,
//! NVIDIA NVENC, AMD AMF) as an asynchronous "MFT". We feed it D3D11 NV12
//! textures directly, so frames never leave video memory before encoding.

mod media_types;
mod tuning;

use std::collections::VecDeque;

use anyhow::{Context, Result, bail};
use bytes::BytesMut;
use pipeline::{EncodedFrame, VideoEncoder};
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::*;
use windows::core::Interface;

use crate::Codec;
use crate::mf;

#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
}

/// Asynchronous hardware encoder taking NV12 textures (see
/// [`crate::BgraToNv12`]) and producing Annex-B access units.
pub struct HwEncoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec_api: Option<ICodecAPI>,
    _device_manager: IMFDXGIDeviceManager,
    fps: u32,
    frame_duration: i64,
    inputs_wanted: u32,
    drained: bool,
    /// Staging area for encoded frames, reused so that a steady stream does
    /// not allocate per frame (see [`read_encoded`]).
    scratch: BytesMut,
    name: String,
}

impl HwEncoder {
    /// Creates the hardware encoder of the GPU identified by `adapter_luid`
    /// (falling back to the best-ranked one), working on `device`'s textures.
    pub fn new(device: &ID3D11Device, adapter_luid: LUID, config: &EncoderConfig) -> Result<Self> {
        let (activate, name) = find_hardware_encoder(config.codec, adapter_luid)?;
        let transform: IMFTransform = unsafe { activate.ActivateObject()? };

        let attributes = unsafe { transform.GetAttributes()? };
        unsafe {
            if attributes.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 0 {
                bail!("{name} is not an asynchronous hardware encoder");
            }
            attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
        }
        let events: IMFMediaEventGenerator = transform.cast()?;
        let device_manager = mf::attach_d3d11_device(&transform, device)?;

        let codec_api = transform.cast::<ICodecAPI>().ok();
        if let Some(api) = &codec_api {
            tuning::configure_low_latency(api, config);
        }
        media_types::set_media_types(&transform, config)?;

        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }

        Ok(Self {
            transform,
            events,
            codec_api,
            _device_manager: device_manager,
            fps: config.fps,
            frame_duration: 10_000_000 / config.fps.max(1) as i64,
            inputs_wanted: 0,
            drained: false,
            scratch: BytesMut::new(),
            name,
        })
    }

    /// The MFT's friendly name, e.g. "NVIDIA H.264 Encoder MFT".
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Flushes the frames still inside the encoder (end of a recording).
    /// The encoder cannot be used afterwards.
    pub fn finish(&mut self, out: &mut VecDeque<EncodedFrame>) -> Result<()> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
        }
        while !self.drained {
            self.poll(true, out)?;
        }
        Ok(())
    }

    fn pull_output(&mut self, out: &mut VecDeque<EncodedFrame>) -> Result<()> {
        let (result, sample) = mf::process_output(&self.transform, None);
        match result {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // The encoder changed its output format; accept its proposal.
                unsafe {
                    let new_type = self.transform.GetOutputAvailableType(0, 0)?;
                    self.transform.SetOutputType(0, &new_type, 0)?;
                }
                return Ok(());
            }
            Err(e) => return Err(e).context("ProcessOutput"),
        }
        let sample = sample.context("encoder produced no sample")?;
        out.push_back(read_encoded(&sample, &mut self.scratch)?);
        Ok(())
    }
}

impl VideoEncoder for HwEncoder {
    type Frame = ID3D11Texture2D;

    /// True when the encoder asked for another frame.
    fn can_accept(&self) -> bool {
        self.inputs_wanted > 0
    }

    /// Queues an NV12 texture. The texture must not be overwritten until the
    /// encoder produced its output (hence the converter's surface pool).
    fn submit(&mut self, nv12: &ID3D11Texture2D, timestamp: i64) -> Result<()> {
        if self.inputs_wanted == 0 {
            bail!("encoder is not ready for input");
        }
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, nv12, 0, false)?;
            // Some encoders reject buffers whose current length is zero.
            if let Ok(buffer_2d) = buffer.cast::<IMF2DBuffer>() {
                buffer.SetCurrentLength(buffer_2d.GetContiguousLength()?)?;
            }
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(timestamp)?;
            sample.SetSampleDuration(self.frame_duration)?;
            self.transform
                .ProcessInput(0, &sample, 0)
                .context("ProcessInput")?;
        }
        self.inputs_wanted -= 1;
        Ok(())
    }

    fn poll(&mut self, block: bool, out: &mut VecDeque<EncodedFrame>) -> Result<()> {
        let mut flags = if block {
            MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)
        } else {
            MF_EVENT_FLAG_NO_WAIT
        };
        loop {
            let event = match unsafe { self.events.GetEvent(flags) } {
                Ok(e) => e,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(e) => return Err(e).context("encoder GetEvent"),
            };
            flags = MF_EVENT_FLAG_NO_WAIT;
            let kind = unsafe { event.GetType()? } as i32;
            if kind == METransformNeedInput.0 {
                self.inputs_wanted += 1;
            } else if kind == METransformHaveOutput.0 {
                self.pull_output(out)?;
            } else if kind == METransformDrainComplete.0 {
                self.drained = true;
            }
        }
    }

    /// Makes the next encoded frame an IDR, e.g. after the viewer lost data.
    fn request_keyframe(&mut self) -> Result<()> {
        let api = self
            .codec_api
            .as_ref()
            .context("encoder has no ICodecAPI")?;
        tuning::force_keyframe(api)?;
        Ok(())
    }

    /// Retargets the CBR rate control mid-stream (`ICodecAPI`). Fails when
    /// the driver refuses: the encoder then keeps its rate.
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        let api = self
            .codec_api
            .as_ref()
            .context("encoder has no ICodecAPI")?;
        tuning::set_rate(api, bitrate_bps, self.fps)
            .with_context(|| format!("{} refused a bitrate of {bitrate_bps} bit/s", self.name))
    }
}

impl Drop for HwEncoder {
    fn drop(&mut self) {
        mf::shutdown(&self.transform);
    }
}

/// Copies the bitstream of an output sample into `scratch` and hands it out
/// as an immutable slice. Once the previous frame has been dropped, `scratch`
/// takes its memory back, so no allocation is needed in steady state.
fn read_encoded(sample: &IMFSample, scratch: &mut BytesMut) -> Result<EncodedFrame> {
    unsafe {
        let timestamp = sample.GetSampleTime().unwrap_or(0);
        let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut ptr, None, Some(&mut len))?;
        // SAFETY: Lock returned `len` readable bytes at `ptr` until Unlock.
        scratch.extend_from_slice(std::slice::from_raw_parts(ptr, len as usize));
        buffer.Unlock()?;
        let data = scratch.split().freeze();
        Ok(EncodedFrame {
            data,
            timestamp,
            keyframe,
        })
    }
}

/// Picks the hardware encoder belonging to the same GPU as our D3D11 device.
fn find_hardware_encoder(codec: Codec, adapter_luid: LUID) -> Result<(IMFActivate, String)> {
    let candidates = mf::enumerate_transforms(
        MFT_CATEGORY_VIDEO_ENCODER,
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
        MFVideoFormat_NV12,
        codec.subtype(),
    )?;
    let ranked = candidates.into_iter().map(|a| {
        let luid = mf::adapter_luid(&a);
        (a, luid)
    });
    let activate = mf::prefer_adapter(ranked.collect(), mf::luid_key(adapter_luid))
        .with_context(|| format!("no hardware {codec:?} encoder found on this PC"))?;
    let name = mf::friendly_name(&activate);
    Ok((activate, name))
}
