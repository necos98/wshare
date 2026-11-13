//! The picture type handed from the decoder to the renderer.
//!
//! It lives in `gpu` (not `codec`) so the renderer can consume decoded frames
//! without depending on the decoder: both sides only know this type
//! (Dependency Inversion).

use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::core::IUnknown;

/// A picture in video memory (normally NV12) that stays valid while held.
///
/// Hardware decoders write into a pool of textures (usually one texture
/// array, one slice per picture) and recycle a slice as soon as the object
/// that owns it is released. `keepalive` holds that owner (for Media
/// Foundation, the output `IMFSample`), so the slice is not overwritten while
/// a `GpuFrame` referencing it exists. Drop the frame promptly: holding many
/// of them starves the decoder's pool.
#[derive(Clone, Debug)]
pub struct GpuFrame {
    /// The texture containing the picture (possibly a texture array).
    pub texture: ID3D11Texture2D,
    /// Subresource index of the picture inside `texture`.
    pub subresource: u32,
    /// Visible width in pixels. The texture may be larger (coded size is
    /// padded to the macroblock size, e.g. 1088 lines for 1080p).
    pub width: u32,
    /// Visible height in pixels.
    pub height: u32,
    /// Presentation timestamp in 100 ns units.
    pub timestamp: i64,
    /// Owner of the texture slice; dropping it lets the producer reuse it.
    pub keepalive: Option<IUnknown>,
}

impl GpuFrame {
    /// Array slice that holds the picture, for views that address slices.
    pub fn array_slice(&self, mip_levels: u32) -> u32 {
        array_slice(self.subresource, mip_levels)
    }
}

/// D3D11 numbers subresources `mip + slice * mip_levels`; returns the slice.
pub fn array_slice(subresource: u32, mip_levels: u32) -> u32 {
    subresource / mip_levels.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_slice_with_single_mip_is_the_subresource() {
        assert_eq!(array_slice(5, 1), 5);
    }

    #[test]
    fn array_slice_divides_by_mip_levels() {
        assert_eq!(array_slice(7, 3), 2);
        assert_eq!(array_slice(6, 3), 2);
    }

    #[test]
    fn array_slice_tolerates_zero_mip_levels() {
        assert_eq!(array_slice(4, 0), 4);
    }
}
