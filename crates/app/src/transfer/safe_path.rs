//! The only door through which a path from the other PC becomes a path on
//! this one. A received path is relative (`folder/sub/file.txt`) and is
//! checked component by component: whatever passes can be joined onto a
//! destination folder and stays inside it, and names exactly one file
//! there (no device, no alternate data stream, no alias Windows would
//! silently trim).

use std::fmt;
use std::path::{Path, PathBuf};

use protocol::transfer::{MAX_PATH_UNITS, PATH_SEPARATOR};

/// Longest single name NTFS accepts, in UTF-16 units.
pub const MAX_COMPONENT_UNITS: usize = 255;

/// Characters Windows does not allow in a name (besides control
/// characters). `:` would also open an alternate data stream, `\` and `/`
/// would be separators.
const FORBIDDEN: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Names that open a device instead of a file, whatever the extension
/// (`NUL.txt` is NUL too). The superscript digits count as digits for
/// `COM` and `LPT`.
const DEVICES: &[&str] = &["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
const NUMBERED_DEVICES: &[&str] = &["COM", "LPT"];
const DEVICE_DIGITS: &[char] = &[
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '\u{b9}', '\u{b2}', '\u{b3}',
];

/// Why a path was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    Empty,
    TooLong,
    /// Absolute, or with an empty, `.` or `..` component.
    NotRelative,
    /// A name Windows would not create as given: forbidden characters, a
    /// device, a trailing dot or space, too long.
    BadName(String),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty path"),
            Self::TooLong => write!(f, "path longer than {MAX_PATH_UNITS} characters"),
            Self::NotRelative => f.write_str("not a relative path"),
            Self::BadName(name) => write!(f, "name not allowed: {name:?}"),
        }
    }
}

impl std::error::Error for PathError {}

/// A relative path whose every component is a plain, valid Windows name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SafePath {
    components: Vec<String>,
}

impl SafePath {
    /// Checks a path as it travels: components separated by `/`.
    pub fn parse(wire: &str) -> Result<Self, PathError> {
        if wire.is_empty() {
            return Err(PathError::Empty);
        }
        if wire.encode_utf16().count() > MAX_PATH_UNITS {
            return Err(PathError::TooLong);
        }
        let components = wire
            .split(PATH_SEPARATOR)
            .map(|component| check_name(component).map(str::to_string))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { components })
    }

    /// A path of one component, checked like any other.
    pub fn name(name: &str) -> Result<Self, PathError> {
        if name.contains(PATH_SEPARATOR) {
            return Err(PathError::BadName(name.to_string()));
        }
        Self::parse(name)
    }

    /// The path as it travels.
    pub fn to_wire(&self) -> String {
        self.components.join(&PATH_SEPARATOR.to_string())
    }

    /// The first component: the item that was dropped or copied.
    pub fn root(&self) -> &str {
        &self.components[0]
    }

    /// Everything but the last component, if there is more than one.
    pub fn parent(&self) -> Option<Self> {
        (self.components.len() > 1).then(|| Self {
            components: self.components[..self.components.len() - 1].to_vec(),
        })
    }

    /// The same path under another first component (a root renamed to
    /// avoid a name already taken). `root` must be a valid name.
    pub fn with_root(&self, root: &str) -> Self {
        let mut components = self.components.clone();
        components[0] = root.to_string();
        Self { components }
    }

    /// `base` followed by this path: always inside `base`.
    pub fn under(&self, base: &Path) -> PathBuf {
        let mut path = base.to_path_buf();
        path.extend(&self.components);
        path
    }

    /// The path with Windows separators, as the shell shows it.
    pub fn to_windows(&self) -> String {
        self.components.join("\\")
    }
}

/// `name` if it is a valid single component.
pub fn check_name(name: &str) -> Result<&str, PathError> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(PathError::NotRelative);
    }
    let bad = || PathError::BadName(name.to_string());
    if name.encode_utf16().count() > MAX_COMPONENT_UNITS {
        return Err(bad());
    }
    if name
        .chars()
        .any(|c| c.is_control() || FORBIDDEN.contains(&c))
    {
        return Err(bad());
    }
    // Windows drops trailing dots and spaces: "a." would be "a".
    if name.ends_with(['.', ' ']) {
        return Err(bad());
    }
    if is_device(name) {
        return Err(bad());
    }
    Ok(name)
}

/// Whether `name` opens a device: its part before the first dot, without
/// trailing spaces, is a device name in any case.
fn is_device(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or_default().trim_end();
    let upper = base.to_uppercase();
    if DEVICES.contains(&upper.as_str()) {
        return true;
    }
    NUMBERED_DEVICES.iter().any(|prefix| {
        upper
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.chars().count() == 1 && rest.starts_with(DEVICE_DIGITS))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(path: &str) -> SafePath {
        SafePath::parse(path).unwrap_or_else(|e| panic!("{path:?}: {e}"))
    }

    #[test]
    fn plain_relative_paths_pass_unchanged() {
        for path in [
            "file.txt",
            "Foto/2024/mare è.jpg",
            ".bashrc",
            "a b/c d",
            "\u{1F5A5}/文件.txt",
            "CONSOLE.txt",
            "COM10",
            "nul0",
            "LPT",
        ] {
            assert_eq!(ok(path).to_wire(), path);
        }
    }

    #[test]
    fn absolute_paths_and_parent_steps_are_refused() {
        for path in [
            "/etc/passwd",
            "a//b",
            "a/",
            "./a",
            "a/./b",
            "..",
            "../a",
            "a/../../b",
            "a/..",
        ] {
            assert_eq!(SafePath::parse(path), Err(PathError::NotRelative), "{path}");
        }
        assert_eq!(SafePath::parse(""), Err(PathError::Empty));
    }

    #[test]
    fn drive_letters_unc_and_backslashes_are_refused() {
        for path in [
            "C:/Windows",
            "C:",
            "c:file",
            "\\\\server\\share",
            "a\\b",
            "..\\..\\x",
            "\\x",
        ] {
            assert!(
                matches!(SafePath::parse(path), Err(PathError::BadName(_))),
                "{path}"
            );
        }
    }

    #[test]
    fn alternate_data_streams_are_refused() {
        for path in ["file.txt:hidden", "file.txt::$DATA", "dir/a:b"] {
            assert!(SafePath::parse(path).is_err(), "{path}");
        }
    }

    #[test]
    fn device_names_are_refused_with_any_extension_or_case() {
        for path in [
            "CON",
            "con",
            "Nul.txt",
            "dir/aux.tar.gz",
            "PRN",
            "COM1",
            "com9.log",
            "LPT3",
            "COM\u{b9}",
            "lpt\u{b2}.txt",
            "NUL .txt",
            "CONIN$",
            "conout$",
        ] {
            assert!(
                matches!(SafePath::parse(path), Err(PathError::BadName(_))),
                "{path}"
            );
        }
    }

    #[test]
    fn forbidden_characters_and_trailing_dots_are_refused() {
        for path in [
            "a<b", "a>b", "a\"b", "a|b", "a?b", "a*b", "a\u{0}b", "a\nb", "a\u{1f}b", "name.",
            "name ", "dir./x", "...",
        ] {
            assert!(SafePath::parse(path).is_err(), "{path:?}");
        }
    }

    #[test]
    fn lengths_are_counted_in_utf16_units() {
        let longest = "x".repeat(MAX_COMPONENT_UNITS);
        assert!(SafePath::parse(&longest).is_ok());
        assert!(SafePath::parse(&format!("{longest}x")).is_err());
        // An emoji is two units: half as many fit.
        let emoji = "\u{1F600}".repeat(MAX_COMPONENT_UNITS / 2 + 1);
        assert!(SafePath::parse(&emoji).is_err());
        let deep = vec!["ab"; MAX_PATH_UNITS / 3 + 1].join("/");
        assert_eq!(SafePath::parse(&deep), Err(PathError::TooLong));
    }

    #[test]
    fn a_safe_path_stays_under_its_base() {
        let base = Path::new("C:\\Users\\a\\Downloads\\wshare");
        let path = ok("Foto/mare.jpg");
        assert_eq!(
            path.under(base),
            PathBuf::from("C:\\Users\\a\\Downloads\\wshare\\Foto\\mare.jpg")
        );
        assert!(path.under(base).starts_with(base));
        assert_eq!(path.to_windows(), "Foto\\mare.jpg");
    }

    #[test]
    fn roots_parents_and_renames() {
        let path = ok("Foto/2024/mare.jpg");
        assert_eq!(path.root(), "Foto");
        assert_eq!(path.parent().unwrap().to_wire(), "Foto/2024");
        assert_eq!(ok("a").parent(), None);
        assert_eq!(
            path.with_root("Foto (1)").to_wire(),
            "Foto (1)/2024/mare.jpg"
        );
    }

    #[test]
    fn a_single_name_has_no_separator() {
        assert!(SafePath::name("a.txt").is_ok());
        assert!(SafePath::name("a/b").is_err());
    }
}
