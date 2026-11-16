//! Versions as in Cargo.toml (`MAJOR.MINOR.PATCH[-PRERELEASE]`), compared
//! so that `1.10.0` > `1.9.0` and a release > its prereleases.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Empty for a release.
    pub pre: String,
}

impl FromStr for Version {
    type Err = String;

    /// Takes an optional leading `v` (a tag) and missing parts as 0 (`1.2`).
    fn from_str(text: &str) -> Result<Self, String> {
        let trimmed = text.trim();
        let bare = trimmed.strip_prefix(['v', 'V']).unwrap_or(trimmed);
        let (numbers, pre) = bare.split_once('-').unwrap_or((bare, ""));
        let mut parts = numbers.split('.');
        let mut next = || -> Result<u64, String> {
            match parts.next() {
                None => Ok(0),
                Some(part) => part.parse().map_err(|_| format!("not a version: {text:?}")),
            }
        };
        let version = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
            pre: pre.to_string(),
        };
        if parts.next().is_some() || numbers.is_empty() {
            return Err(format!("not a version: {text:?}"));
        }
        Ok(version)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre)?;
        }
        Ok(())
    }
}

/// Whether `candidate` is newer than `current`; `false` if either is not a
/// version (an unreadable manifest never triggers an update).
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (current.parse::<Version>(), candidate.parse::<Version>()) {
        (Ok(current), Ok(candidate)) => candidate > current,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        text.parse().unwrap()
    }

    #[test]
    fn numbers_compare_as_numbers() {
        assert!(v("1.10.0") > v("1.9.0"));
        assert!(v("0.2.0") > v("0.1.9"));
        assert!(v("2.0.0") > v("1.99.99"));
    }

    #[test]
    fn a_release_is_newer_than_its_prereleases() {
        assert!(v("1.0.0") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.2") > v("1.0.0-rc.1"));
    }

    #[test]
    fn tags_and_short_forms_parse() {
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        assert_eq!(v("1.2"), v("1.2.0"));
        assert_eq!(v("1.2.3-beta").to_string(), "1.2.3-beta");
    }

    #[test]
    fn garbage_is_not_a_version() {
        for text in ["", "v", "uno", "1.2.3.4", "1..2", "1.x"] {
            assert!(text.parse::<Version>().is_err(), "{text:?}");
        }
    }

    #[test]
    fn only_a_strictly_newer_version_is_an_update() {
        assert!(is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.2.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "rotto"));
    }
}
