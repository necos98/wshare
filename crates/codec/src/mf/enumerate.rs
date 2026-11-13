//! Finding the MFTs (encoders/decoders) installed on the system.

use anyhow::Result;
use windows::Win32::Foundation::LUID;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, MFMediaType_Video, MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG,
    MFT_FRIENDLY_NAME_Attribute, MFT_REGISTER_TYPE_INFO, MFTEnumEx,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::{GUID, PWSTR};

/// Lists the MFTs of `category` converting video subtype `input` into
/// `output`, best-ranked first when `flags` include SORTANDFILTER.
pub(crate) fn enumerate_transforms(
    category: GUID,
    flags: MFT_ENUM_FLAG,
    input: GUID,
    output: GUID,
) -> Result<Vec<IMFActivate>> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: input,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: output,
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnumEx(
            category,
            flags,
            Some(&input),
            Some(&output),
            &mut activates,
            &mut count,
        )?
    };
    if activates.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: MFTEnumEx returned a CoTaskMem array of `count` references; we
    // move them out before freeing the array itself.
    let list = unsafe {
        let list = std::slice::from_raw_parts_mut(activates, count as usize)
            .iter_mut()
            .filter_map(Option::take)
            .collect();
        CoTaskMemFree(Some(activates as *const _));
        list
    };
    Ok(list)
}

/// Human-readable MFT name, e.g. "NVIDIA H.264 Encoder MFT".
pub(crate) fn friendly_name(activate: &IMFActivate) -> String {
    let mut text = PWSTR::null();
    let mut len = 0u32;
    let found =
        unsafe { activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut text, &mut len) };
    if found.is_err() {
        return "unknown MFT".into();
    }
    // SAFETY: on success `text` is a NUL-terminated CoTaskMem string we own.
    unsafe {
        let name = text.to_string().unwrap_or_default();
        CoTaskMemFree(Some(text.0 as *const _));
        name
    }
}

/// The GPU a hardware MFT belongs to, if it reports one.
pub(crate) fn adapter_luid(activate: &IMFActivate) -> Option<u64> {
    unsafe { activate.GetUINT64(&MFT_ENUM_ADAPTER_LUID) }.ok()
}

/// The `u64` form of a LUID, as stored in `MFT_ENUM_ADAPTER_LUID`.
pub(crate) fn luid_key(luid: LUID) -> u64 {
    ((luid.HighPart as u32 as u64) << 32) | luid.LowPart as u64
}

/// Picks the candidate on adapter `wanted`, otherwise the first one (the
/// best-ranked, since enumeration is sorted). On hybrid laptops this keeps
/// the MFT on the same GPU as our textures, avoiding cross-adapter copies.
pub(crate) fn prefer_adapter<T>(candidates: Vec<(T, Option<u64>)>, wanted: u64) -> Option<T> {
    let mut first = None;
    for (item, luid) in candidates {
        if luid == Some(wanted) {
            return Some(item);
        }
        first.get_or_insert(item);
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefer_adapter_picks_matching_luid() {
        let picked = prefer_adapter(vec![("a", Some(1)), ("b", Some(2)), ("c", None)], 2);
        assert_eq!(picked, Some("b"));
    }

    #[test]
    fn prefer_adapter_falls_back_to_first() {
        let picked = prefer_adapter(vec![("a", None), ("b", Some(3))], 7);
        assert_eq!(picked, Some("a"));
    }

    #[test]
    fn prefer_adapter_of_nothing_is_none() {
        assert_eq!(prefer_adapter::<&str>(vec![], 1), None);
    }

    #[test]
    fn luid_key_puts_high_part_on_top() {
        let luid = LUID {
            LowPart: 0x1234,
            HighPart: 0x5,
        };
        assert_eq!(luid_key(luid), 0x5_0000_1234);
    }
}
