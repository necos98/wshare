//! Rate control and latency settings applied through `ICodecAPI`.

use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Variant::VARIANT;
use windows::core::GUID;

use super::EncoderConfig;
use crate::mf::{variant_bool, variant_u32};

/// Every setting is a hint: vendors support different subsets, so failures
/// are ignored rather than fatal.
pub(super) fn configure_low_latency(api: &ICodecAPI, config: &EncoderConfig) {
    let set = |key: &GUID, value: VARIANT| unsafe {
        let _ = api.SetValue(key, &value);
    };
    set(&CODECAPI_AVLowLatencyMode, variant_bool(true));
    set(
        &CODECAPI_AVEncCommonRateControlMode,
        variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32),
    );
    set(
        &CODECAPI_AVEncCommonMeanBitRate,
        variant_u32(config.bitrate_bps),
    );
    // In a LAN short bursts are harmless, so allow a few frames' worth of
    // buffer: big screen changes stay sharp instead of turning blocky.
    set(
        &CODECAPI_AVEncCommonBufferSize,
        variant_u32(vbv_buffer_bits(config)),
    );
    // No B-frames: each one adds a frame of delay (and the decoder relies on
    // output order == input order to emit pictures immediately).
    set(&CODECAPI_AVEncMPVDefaultBPictureCount, variant_u32(0));
    // Rare periodic keyframes; the client asks for one when it needs it.
    set(
        &CODECAPI_AVEncMPVGOPSize,
        variant_u32(config.fps.saturating_mul(10)),
    );
    set(&CODECAPI_AVEncCommonQualityVsSpeed, variant_u32(50));
}

/// Forces the next encoded frame to be an IDR.
pub(super) fn force_keyframe(api: &ICodecAPI) -> windows::core::Result<()> {
    unsafe { api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_u32(1)) }
}

/// Rate-control buffer: four frames' worth of bits.
fn vbv_buffer_bits(config: &EncoderConfig) -> u32 {
    (config.bitrate_bps / config.fps.max(1)).saturating_mul(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Codec;

    #[test]
    fn vbv_buffer_is_four_frames() {
        let config = EncoderConfig {
            codec: Codec::H264,
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_bps: 20_000_000,
        };
        assert_eq!(vbv_buffer_bits(&config), 20_000_000 / 60 * 4);
    }

    #[test]
    fn vbv_buffer_survives_zero_fps() {
        let config = EncoderConfig {
            codec: Codec::Hevc,
            width: 64,
            height: 64,
            fps: 0,
            bitrate_bps: 1000,
        };
        assert_eq!(vbv_buffer_bits(&config), 4000);
    }
}
