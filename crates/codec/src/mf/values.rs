//! Encoding of attribute values: MF packs pairs (frame size, frame rate,
//! aspect ratio) into one `u64`, and `ICodecAPI` takes `VARIANT`s.

use windows::Win32::Foundation::VARIANT_TRUE;
use windows::Win32::System::Variant::{VARIANT, VARIANT_0_0, VT_BOOL, VT_UI4};

/// Packs two 32-bit values the way `MFSetAttributeSize`/`Ratio` do: the
/// first one in the high half.
pub(crate) fn pack_pair(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

/// Inverse of [`pack_pair`].
pub(crate) fn unpack_pair(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

pub(crate) fn variant_u32(value: u32) -> VARIANT {
    let mut v = VARIANT::default();
    // SAFETY: sets the tag and the matching member of a zeroed VARIANT.
    unsafe {
        let inner: &mut VARIANT_0_0 = &mut v.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = value;
    }
    v
}

pub(crate) fn variant_bool(value: bool) -> VARIANT {
    let mut v = VARIANT::default();
    // SAFETY: as above.
    unsafe {
        let inner: &mut VARIANT_0_0 = &mut v.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = if value {
            VARIANT_TRUE
        } else {
            Default::default()
        };
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_pair_puts_first_value_high() {
        assert_eq!(pack_pair(1920, 1080), (1920u64 << 32) | 1080);
    }

    #[test]
    fn unpack_pair_roundtrips() {
        assert_eq!(unpack_pair(pack_pair(3840, 2160)), (3840, 2160));
        assert_eq!(unpack_pair(pack_pair(u32::MAX, 0)), (u32::MAX, 0));
    }

    #[test]
    fn variant_u32_sets_tag_and_value() {
        let v = variant_u32(42);
        let inner = unsafe { &v.Anonymous.Anonymous };
        assert_eq!(inner.vt, VT_UI4);
        assert_eq!(unsafe { inner.Anonymous.ulVal }, 42);
    }

    #[test]
    fn variant_bool_uses_com_true() {
        let v = variant_bool(true);
        let inner = unsafe { &v.Anonymous.Anonymous };
        assert_eq!(inner.vt, VT_BOOL);
        assert_eq!(unsafe { inner.Anonymous.boolVal }, VARIANT_TRUE);
    }
}
