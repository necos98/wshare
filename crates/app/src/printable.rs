//! Text that came from the other PC, made safe to keep and to print. The
//! terminal lines and the typed lines read by the desktop interface are
//! one line each: a line break, or any other control character, in a name
//! or a reason chosen by the other PC must never end one early and start a
//! line of its own.

/// `text` on one line: every control character becomes a space, so the
/// words around it stay apart.
pub fn single_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// `text` without its control characters, at most `max_chars` characters
/// long: what is kept of a text the other PC sent.
pub fn clean(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_break_becomes_a_space() {
        assert_eq!(
            single_line("A\r\n@wshare:failed:wrong-pin\t\u{1b}x"),
            "A  @wshare:failed:wrong-pin  x"
        );
        assert_eq!(single_line("Città — sala 2"), "Città — sala 2");
        assert_eq!(single_line(""), "");
    }

    #[test]
    fn a_clean_text_has_no_control_characters_and_a_bounded_length() {
        assert_eq!(clean("PORTATILE\n\u{1b}[31m", 64), "PORTATILE[31m");
        assert_eq!(clean(&"è".repeat(100), 10), "è".repeat(10));
        assert_eq!(clean("a\u{85}b", 10), "ab");
        assert_eq!(clean("", 10), "");
    }
}
