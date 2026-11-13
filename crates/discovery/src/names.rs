//! DNS naming: host names, instance names and mapping instance names back to host ids.

use crate::SERVICE_TYPE;
use crate::txt::{is_valid_id, truncate_utf8};

/// Maximum length of a single DNS label in bytes.
const MAX_LABEL_LEN: usize = 63;
/// Label used when sanitization leaves nothing.
const FALLBACK_LABEL: &str = "wshare";

/// Name of this computer: `%COMPUTERNAME%` on Windows, otherwise the OS host name,
/// otherwise `"wshare"`. Suitable as [`crate::HostAnnouncement::display_name`].
pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| gethostname::gethostname().into_string().ok())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| FALLBACK_LABEL.to_owned())
}

/// Turns arbitrary text into a valid DNS label: lowercase ASCII letters, digits and single
/// inner hyphens, at most 63 bytes. Returns `"wshare"` if nothing usable remains.
pub fn sanitize_dns_label(name: &str) -> String {
    let mut out = String::with_capacity(name.len().min(MAX_LABEL_LEN));
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(MAX_LABEL_LEN);
    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() {
        FALLBACK_LABEL.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// mDNS host name (A/AAAA record owner), e.g. `"desk-pc-a1b2c3.local."`.
///
/// The id is part of the name so that two PCs with the same computer name do not publish
/// conflicting address records, and so we never collide with the `<computername>.local`
/// record published by Windows' built-in mDNS responder.
pub fn host_name(computer_name: &str, id: &str) -> String {
    let id_label = sanitize_dns_label(id);
    let max_prefix = MAX_LABEL_LEN.saturating_sub(id_label.len() + 1).max(1);
    let prefix = sanitize_dns_label(computer_name);
    let prefix = prefix[..prefix.len().min(max_prefix)].trim_end_matches('-');
    format!("{prefix}-{id_label}.local.")
}

/// DNS-SD instance name (`"{display_name}-{id}"`), unique on the LAN thanks to the id.
///
/// The display part is truncated so the label fits in 63 bytes; control characters, `.` and
/// `\` are replaced to avoid DNS escaping. The full display name travels in TXT instead.
pub fn instance_name(display_name: &str, id: &str) -> String {
    let cleaned: String = display_name
        .chars()
        .map(|c| {
            if c.is_control() || c == '.' || c == '\\' {
                '_'
            } else {
                c
            }
        })
        .collect();
    let max_display = MAX_LABEL_LEN.saturating_sub(id.len() + 1);
    format!("{}-{id}", truncate_utf8(cleaned.trim(), max_display))
}

/// Extracts the host id from a service fullname such as `"Desk PC-a1b2._wshare._udp.local."`.
/// Returns `None` if the name is not ours or does not end in a valid id.
pub(crate) fn id_from_fullname(fullname: &str) -> Option<&str> {
    let instance = fullname.strip_suffix(SERVICE_TYPE)?.strip_suffix('.')?;
    let (_, id) = instance.rsplit_once('-')?;
    is_valid_id(id).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_dns_label("DESKTOP-AB12CD"), "desktop-ab12cd");
        assert_eq!(sanitize_dns_label("  Jacob's  PC!! "), "jacob-s-pc");
        assert_eq!(sanitize_dns_label("Büro_PC"), "b-ro-pc");
        assert_eq!(sanitize_dns_label("---"), "wshare");
        assert_eq!(sanitize_dns_label("日本"), "wshare");
        let long = sanitize_dns_label(&"a".repeat(100));
        assert_eq!(long.len(), 63);
        // A truncation that ends on a hyphen is trimmed.
        let s = sanitize_dns_label(&format!("{}-b", "a".repeat(62)));
        assert_eq!(s, "a".repeat(62));
    }

    #[test]
    fn host_name_includes_id_and_fits_label() {
        assert_eq!(host_name("DESKTOP-1", "A1b2"), "desktop-1-a1b2.local.");
        let h = host_name(&"x".repeat(100), "abcdef");
        let label = h.strip_suffix(".local.").unwrap();
        assert_eq!(label.len(), 63);
        assert!(label.ends_with("-abcdef"));
        assert_eq!(host_name("!!", "ab"), "wshare-ab.local.");
    }

    #[test]
    fn instance_name_roundtrips_to_id() {
        for display in ["Desk PC", "a-b-c", "dots.in.name", "", "back\\slash", "ü"] {
            let inst = instance_name(display, "a1b2c3");
            let fullname = format!("{inst}.{SERVICE_TYPE}");
            assert_eq!(id_from_fullname(&fullname), Some("a1b2c3"), "{display:?}");
        }
    }

    #[test]
    fn instance_name_is_truncated() {
        let inst = instance_name(&"é".repeat(100), "0123456789abcdef");
        assert!(inst.len() <= 63);
        assert!(inst.ends_with("-0123456789abcdef"));
    }

    #[test]
    fn id_from_foreign_or_malformed_names() {
        assert_eq!(id_from_fullname("PC-abc._http._tcp.local."), None);
        assert_eq!(id_from_fullname("noid._wshare._udp.local."), None);
        assert_eq!(id_from_fullname("PC-abc (2)._wshare._udp.local."), None);
        assert_eq!(id_from_fullname("PC-._wshare._udp.local."), None);
    }

    #[test]
    fn computer_name_is_not_empty() {
        assert!(!computer_name().is_empty());
    }
}
