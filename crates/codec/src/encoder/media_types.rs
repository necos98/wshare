//! Negotiating the encoder's input (NV12) and output (H.264/HEVC) formats.

use anyhow::{Context, Result};
use windows::Win32::Media::MediaFoundation::*;

use super::EncoderConfig;
use crate::Codec;
use crate::mf::pack_pair;

pub(super) fn set_media_types(transform: &IMFTransform, config: &EncoderConfig) -> Result<()> {
    let frame_size = pack_pair(config.width, config.height);
    let frame_rate = pack_pair(config.fps, 1);
    unsafe {
        // Encoders need the output type before the input type.
        let output = MFCreateMediaType()?;
        output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        output.SetGUID(&MF_MT_SUBTYPE, &config.codec.subtype())?;
        output.SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate_bps)?;
        output.SetUINT64(&MF_MT_FRAME_SIZE, frame_size)?;
        output.SetUINT64(&MF_MT_FRAME_RATE, frame_rate)?;
        output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_pair(1, 1))?;
        output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        output.SetUINT32(&MF_MT_MPEG2_PROFILE, profile(config.codec))?;
        // Matches the colour space the video processor writes (BT.709,
        // limited range); the decoder side assumes the same.
        output.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        output.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        output.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        output.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        transform
            .SetOutputType(0, &output, 0)
            .context("encoder rejected the output format (resolution/codec unsupported?)")?;

        for index in 0.. {
            let input = transform
                .GetInputAvailableType(0, index)
                .context("encoder does not accept NV12 input")?;
            if input.GetGUID(&MF_MT_SUBTYPE)? != MFVideoFormat_NV12 {
                continue;
            }
            input.SetUINT64(&MF_MT_FRAME_SIZE, frame_size)?;
            input.SetUINT64(&MF_MT_FRAME_RATE, frame_rate)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            transform
                .SetInputType(0, &input, 0)
                .context("SetInputType")?;
            break;
        }
    }
    Ok(())
}

fn profile(codec: Codec) -> u32 {
    match codec {
        Codec::H264 => eAVEncH264VProfile_High.0 as u32,
        Codec::Hevc => eAVEncH265VProfile_Main_420_8.0 as u32,
    }
}
