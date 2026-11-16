//! The clipboard's text format, `CF_UNICODETEXT`: UTF-16 ending with a NUL.

use protocol::control::MAX_CLIPBOARD_BYTES;

use super::rules::ClipboardContent;

/// The text in `units`, up to the first NUL (or all of them: the memory
/// behind a clipboard handle may be larger than the text).
///
/// Unpaired surrogates become U+FFFD. The size is checked before any
/// conversion, so a huge text is never copied: a UTF-16 unit is at least
/// one UTF-8 byte, so more units than the limit can only be too much.
pub fn decode(units: &[u16]) -> ClipboardContent {
    let end = units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len());
    let units = &units[..end];
    if units.is_empty() {
        return ClipboardContent::NoText;
    }
    if units.len() > MAX_CLIPBOARD_BYTES {
        return ClipboardContent::TooLarge;
    }
    let text = String::from_utf16_lossy(units);
    if text.len() > MAX_CLIPBOARD_BYTES {
        return ClipboardContent::TooLarge;
    }
    ClipboardContent::Text(text)
}

/// `text` as UTF-16 with its terminating NUL. Text after a NUL is dropped:
/// the clipboard's text ends there anyway.
pub fn encode(text: &str) -> Vec<u16> {
    let text = text.split('\0').next().unwrap_or_default();
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn text_roundtrips_with_accents_emoji_and_line_breaks() {
        for text in ["ciao", "è già così", "a\u{1F5A5}b", "riga 1\r\nriga 2"] {
            assert_eq!(decode(&encode(text)), ClipboardContent::Text(text.into()));
        }
    }

    #[test]
    fn the_encoding_ends_with_a_nul() {
        assert_eq!(encode("ab"), [0x61, 0x62, 0]);
        assert_eq!(encode(""), [0]);
    }

    #[test]
    fn text_after_a_nul_is_left_out_both_ways() {
        assert_eq!(encode("ab\0cd"), [0x61, 0x62, 0]);
        let mut raw = units("ab");
        raw.extend([0, 0x63, 0x64]);
        assert_eq!(decode(&raw), ClipboardContent::Text("ab".into()));
    }

    #[test]
    fn memory_without_a_terminator_is_read_whole() {
        assert_eq!(decode(&units("ab")), ClipboardContent::Text("ab".into()));
    }

    #[test]
    fn empty_memory_holds_no_text() {
        assert_eq!(decode(&[]), ClipboardContent::NoText);
        assert_eq!(decode(&[0, 0x61]), ClipboardContent::NoText);
    }

    #[test]
    fn broken_surrogates_become_replacement_characters() {
        assert_eq!(
            decode(&[0x61, 0xD800, 0x62]),
            ClipboardContent::Text("a\u{FFFD}b".into())
        );
    }

    #[test]
    fn the_limit_counts_utf8_bytes() {
        let at_limit = "x".repeat(MAX_CLIPBOARD_BYTES);
        assert_eq!(decode(&units(&at_limit)), ClipboardContent::Text(at_limit));
        // Few enough units, but two bytes each in UTF-8.
        let accented = "è".repeat(MAX_CLIPBOARD_BYTES / 2 + 1);
        assert_eq!(decode(&units(&accented)), ClipboardContent::TooLarge);
        let long = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        assert_eq!(decode(&units(&long)), ClipboardContent::TooLarge);
    }
}
