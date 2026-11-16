//! A free name for something received: the name itself, or, when taken,
//! `nome (1).ext`, `nome (2).ext`... as Explorer does.

use super::safe_path::{MAX_COMPONENT_UNITS, check_name};

/// How many numbered names are tried before giving up.
const MAX_ATTEMPTS: u32 = 9999;

/// The first of `name`, `stem (1).ext`, `stem (2).ext`... that `taken`
/// says is free; `None` if every attempt is taken. Folders keep their
/// whole name as the stem (`foto.2024 (1)`); for files the extension is
/// what follows the last dot, unless the name starts with it (`.bashrc`).
/// The stem is shortened when needed so the name stays valid.
pub fn unique_name(name: &str, is_dir: bool, taken: impl Fn(&str) -> bool) -> Option<String> {
    if !taken(name) {
        return Some(name.to_string());
    }
    let (stem, extension) = split_extension(name, is_dir);
    (1..=MAX_ATTEMPTS)
        .map(|n| numbered(stem, extension, n))
        .find(|candidate| check_name(candidate).is_ok() && !taken(candidate))
}

fn split_extension(name: &str, is_dir: bool) -> (&str, &str) {
    if is_dir {
        return (name, "");
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    }
}

/// `stem (n)extension`, with the stem cut (never inside a character) so
/// the whole stays within [`MAX_COMPONENT_UNITS`].
fn numbered(stem: &str, extension: &str, n: u32) -> String {
    let suffix = format!(" ({n}){extension}");
    let room = MAX_COMPONENT_UNITS.saturating_sub(suffix.encode_utf16().count());
    let mut units = 0;
    let stem: String = stem
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= room
        })
        .collect();
    // A cut may leave a trailing space or dot, which Windows would drop.
    format!("{}{suffix}", stem.trim_end_matches(['.', ' ']))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn taken(names: &[&str]) -> impl Fn(&str) -> bool {
        let set: HashSet<String> = names.iter().map(|n| n.to_string()).collect();
        move |name| set.contains(name)
    }

    #[test]
    fn a_free_name_is_kept() {
        assert_eq!(
            unique_name("a.txt", false, taken(&[])),
            Some("a.txt".into())
        );
    }

    #[test]
    fn a_taken_name_gets_the_first_free_number_before_the_extension() {
        assert_eq!(
            unique_name("foto.jpg", false, taken(&["foto.jpg"])),
            Some("foto (1).jpg".into())
        );
        assert_eq!(
            unique_name(
                "foto.jpg",
                false,
                taken(&["foto.jpg", "foto (1).jpg", "foto (2).jpg"])
            ),
            Some("foto (3).jpg".into())
        );
    }

    #[test]
    fn only_the_last_extension_moves() {
        assert_eq!(
            unique_name("archivio.tar.gz", false, taken(&["archivio.tar.gz"])),
            Some("archivio.tar (1).gz".into())
        );
    }

    #[test]
    fn folders_and_dot_files_keep_their_whole_name() {
        assert_eq!(
            unique_name("foto.2024", true, taken(&["foto.2024"])),
            Some("foto.2024 (1)".into())
        );
        assert_eq!(
            unique_name(".bashrc", false, taken(&[".bashrc"])),
            Some(".bashrc (1)".into())
        );
        assert_eq!(
            unique_name("Documenti", true, taken(&["Documenti"])),
            Some("Documenti (1)".into())
        );
    }

    #[test]
    fn a_long_name_is_shortened_to_stay_valid() {
        let long = format!("{}.txt", "è".repeat(MAX_COMPONENT_UNITS - 4));
        let name = unique_name(&long, false, |n| n == long).unwrap();
        assert!(name.ends_with(" (1).txt"), "{name}");
        assert!(name.encode_utf16().count() <= MAX_COMPONENT_UNITS);
        assert!(check_name(&name).is_ok());
    }

    #[test]
    fn a_cut_never_leaves_a_trailing_dot_or_space() {
        // " (1)" leaves room for 251 units: the cut falls right after the
        // space.
        let stem = format!("{} {}", "a".repeat(MAX_COMPONENT_UNITS - 5), "b".repeat(10));
        let name = numbered(&stem, "", 1);
        assert_eq!(name, format!("{} (1)", "a".repeat(MAX_COMPONENT_UNITS - 5)));
        assert!(check_name(&name).is_ok(), "{name:?}");
    }

    #[test]
    fn everything_taken_gives_up() {
        assert_eq!(unique_name("a", false, |_| true), None);
    }
}
