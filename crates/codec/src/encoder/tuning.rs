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
    set(
        &CODECAPI_AVEncMPVGOPSize,
        variant_u32(gop_frames(config.fps)),
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

/// Time between two keyframes the encoder inserts on its own. Lost video is
/// recovered by resending datagrams, and the viewer asks for a keyframe
/// when that fails, so periodic keyframes are no longer how the picture
/// heals: each one is a burst of several frames' worth of data, a regular
/// spike of latency and loss. One a minute remains as a safety net against
/// damage the viewer cannot notice (a frame decoded from a corrupt
/// reference), rare enough to cost nothing. Not 0: drivers read it
/// differently (no periodic keyframe, or a default of their own).
const GOP_SECONDS: u32 = 60;

/// GOP length in frames for `fps`.
fn gop_frames(fps: u32) -> u32 {
    fps.saturating_mul(GOP_SECONDS)
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
    fn periodic_keyframes_come_once_a_minute() {
        assert_eq!(gop_frames(60), 3600);
        assert_eq!(gop_frames(u32::MAX), u32::MAX);
    }

    #[test]
    fn vbv_buffer_survives_zero_fps() {
        assert_eq!(vbv_buffer_bits(1000, 0), 4000);
    }
}
