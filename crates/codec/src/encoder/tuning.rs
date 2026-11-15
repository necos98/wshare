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
    let _ = set_rate(api, config.bitrate_bps, config.fps);
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

/// Sets the CBR target and the rate-control buffer that goes with it; both
/// may change while streaming. Fails only when the target is refused: a
/// driver that keeps its buffer size merely allows larger bursts.
pub(super) fn set_rate(api: &ICodecAPI, bitrate_bps: u32, fps: u32) -> windows::core::Result<()> {
    unsafe {
        api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(bitrate_bps))?;
        // In a LAN short bursts are harmless, so allow a few frames' worth
        // of buffer: big screen changes stay sharp instead of turning blocky.
        let _ = api.SetValue(
            &CODECAPI_AVEncCommonBufferSize,
            &variant_u32(vbv_buffer_bits(bitrate_bps, fps)),
        );
    }
    Ok(())
}

/// Forces the next encoded frame to be an IDR.
pub(super) fn force_keyframe(api: &ICodecAPI) -> windows::core::Result<()> {
    unsafe { api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_u32(1)) }
}

/// Rate-control buffer: four frames' worth of bits.
fn vbv_buffer_bits(bitrate_bps: u32, fps: u32) -> u32 {
    (bitrate_bps / fps.max(1)).saturating_mul(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vbv_buffer_is_four_frames() {
        assert_eq!(vbv_buffer_bits(20_000_000, 60), 20_000_000 / 60 * 4);
    }

    #[test]
    fn vbv_buffer_follows_the_rate() {
        assert_eq!(vbv_buffer_bits(1_500_000, 60), 1_500_000 / 60 * 4);
    }

    #[test]
    fn vbv_buffer_survives_zero_fps() {
        assert_eq!(vbv_buffer_bits(1000, 0), 4000);
    }
}
