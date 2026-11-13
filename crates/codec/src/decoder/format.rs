//! Media type negotiation for the decoder: compressed input, NV12 output,
//! and working out the visible picture size.

use anyhow::{Context, Result, bail};
use windows::Win32::Media::MediaFoundation::*;

use crate::Codec;
use crate::mf::{pack_pair, unpack_pair};

/// What the decoder currently outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OutputFormat {
    /// Allocated (coded) size of the output surfaces, e.g. 1920x1088.
    pub coded: (u32, u32),
    /// Visible size, e.g. 1920x1080.
    pub display: (u32, u32),
}

/// Declares the compressed stream. The size is only a hint: the decoder
/// reads the real one from the SPS and reports it through a stream change.
pub(super) fn set_input_type(
    transform: &IMFTransform,
    codec: Codec,
    size: (u32, u32),
) -> Result<()> {
    unsafe {
        let input = MFCreateMediaType()?;
        input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        input.SetGUID(&MF_MT_SUBTYPE, &codec.subtype())?;
        input.SetUINT64(&MF_MT_FRAME_SIZE, pack_pair(size.0, size.1))?;
        input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        transform
            .SetInputType(0, &input, 0)
            .with_context(|| format!("decoder rejected {codec:?} input"))?;
    }
    Ok(())
}

/// Selects the decoder's NV12 output type (initially and after every
/// `MF_E_TRANSFORM_STREAM_CHANGE`) and returns its geometry. `requested` is
/// the size the caller announced, used when the type has no display aperture.
pub(super) fn select_nv12_output(
    transform: &IMFTransform,
    requested: (u32, u32),
) -> Result<OutputFormat> {
    for index in 0.. {
        let candidate = match unsafe { transform.GetOutputAvailableType(0, index) } {
            Ok(t) => t,
            Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
            Err(e) => return Err(e).context("GetOutputAvailableType"),
        };
        if unsafe { candidate.GetGUID(&MF_MT_SUBTYPE)? } != MFVideoFormat_NV12 {
            continue;
        }
        unsafe {
            transform
                .SetOutputType(0, &candidate, 0)
                .context("SetOutputType(NV12)")?
        };
        let coded = unpack_pair(unsafe { candidate.GetUINT64(&MF_MT_FRAME_SIZE)? });
        let display = display_size(minimum_display_aperture(&candidate), coded, requested);
        return Ok(OutputFormat { coded, display });
    }
    bail!("decoder offers no NV12 output")
}

/// Reads `MF_MT_MINIMUM_DISPLAY_APERTURE` (the SPS cropping window).
fn minimum_display_aperture(media_type: &IMFMediaType) -> Option<(u32, u32)> {
    let mut area = MFVideoArea::default();
    // SAFETY: MFVideoArea is plain data; the blob is copied into its bytes
    // only if it has exactly that size.
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            (&mut area as *mut MFVideoArea).cast::<u8>(),
            size_of::<MFVideoArea>(),
        )
    };
    let mut written = 0u32;
    unsafe { media_type.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, Some(&mut written)) }
        .ok()?;
    if written as usize != size_of::<MFVideoArea>() {
        return None;
    }
    Some((area.Area.cx.max(0) as u32, area.Area.cy.max(0) as u32))
}

/// Chooses the visible picture size: the stream's aperture if it reports a
/// usable one; otherwise the requested size if it is the coded size minus
/// block padding (coded sizes are rounded up to 16/32/64-pixel blocks);
/// otherwise the coded size (the stream resolution changed, so the requested
/// size no longer describes it).
pub(super) fn display_size(
    aperture: Option<(u32, u32)>,
    coded: (u32, u32),
    requested: (u32, u32),
) -> (u32, u32) {
    let fits = |(w, h): (u32, u32)| w > 0 && h > 0 && w <= coded.0 && h <= coded.1;
    match aperture {
        Some(size) if fits(size) => size,
        _ if fits(requested) && coded.0 - requested.0 < 64 && coded.1 - requested.1 < 64 => {
            requested
        }
        _ => coded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aperture_wins_when_valid() {
        assert_eq!(
            display_size(Some((1920, 1080)), (1920, 1088), (1280, 720)),
            (1920, 1080)
        );
    }

    #[test]
    fn requested_size_is_used_without_aperture() {
        assert_eq!(display_size(None, (1920, 1088), (1920, 1080)), (1920, 1080));
    }

    #[test]
    fn empty_or_oversized_aperture_is_ignored() {
        assert_eq!(
            display_size(Some((0, 0)), (1920, 1088), (1920, 1080)),
            (1920, 1080)
        );
        assert_eq!(
            display_size(Some((4000, 1080)), (1920, 1088), (1920, 1080)),
            (1920, 1080)
        );
    }

    #[test]
    fn coded_size_is_used_when_stream_resolution_changed() {
        // Announced 1080p but the stream is now 720p.
        assert_eq!(display_size(None, (1280, 720), (1920, 1080)), (1280, 720));
        // Announced 1080p but the stream is now 4K: 1080p would crop it.
        assert_eq!(display_size(None, (3840, 2160), (1920, 1080)), (3840, 2160));
    }
}
