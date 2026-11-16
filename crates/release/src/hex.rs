//! Lowercase hex, the text form of keys, signatures and hashes.

/// `bytes` as lowercase hex.
pub fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// The bytes of `text` (hex, either case, surrounding whitespace ignored);
/// `None` if it is not hex.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    // A BOM is not whitespace for `trim`, but a key pasted through a
    // Windows shell may start with one.
    let text = text.trim().trim_start_matches('\u{feff}').trim().as_bytes();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.chunks(2)
        .map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?))
        .collect()
}

fn digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let bytes = [0x00, 0x7f, 0x80, 0xff, 0x12];
        assert_eq!(encode(&bytes), "007f80ff12");
        assert_eq!(decode("007f80ff12").unwrap(), bytes);
        assert_eq!(decode(" 007F80FF12\n").unwrap(), bytes);
    }

    #[test]
    fn rejects_what_is_not_hex() {
        assert_eq!(decode("abc"), None);
        assert_eq!(decode("zz"), None);
        assert_eq!(decode("0g"), None);
        assert_eq!(decode("").unwrap(), Vec::<u8>::new());
    }
}
