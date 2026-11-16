//! `latest.json`: the version of a release and its files.
//!
//! ```json
//! {
//!   "version": "0.2.0",
//!   "notes": "wshare v0.2.0",
//!   "pub_date": "2026-11-17T12:00:00Z",
//!   "files": [
//!     { "name": "wshare.exe", "url": "https://github.com/.../wshare.exe",
//!       "sha256": "…", "size": 12345678 }
//!   ]
//! }
//! ```
//!
//! Only read after its signature is verified (see the crate docs); the
//! checks here keep even a signed manifest from naming a path outside the
//! program's folder or a plain-HTTP download.

use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::hex;
use crate::version::Version;

const SHA256_HEX_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pub_date: String,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// A bare file name: where it goes, next to the running wshare.exe.
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("the manifest is not valid JSON: {0}")]
    Json(String),
    #[error("the manifest's version is not a version: {0:?}")]
    Version(String),
    #[error("the manifest names an unsafe file: {0:?}")]
    UnsafeName(String),
    #[error("the manifest's download is not HTTPS: {0:?}")]
    InsecureUrl(String),
    #[error("the manifest's hash of {0} is not SHA-256 hex")]
    BadHash(String),
}

impl Manifest {
    /// Parses and checks a manifest (whose signature was verified).
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        let manifest: Self =
            serde_json::from_slice(bytes).map_err(|e| ManifestError::Json(e.to_string()))?;
        manifest.check()?;
        Ok(manifest)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a manifest always serializes")
    }

    pub fn file(&self, name: &str) -> Option<&FileEntry> {
        self.files
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(name))
    }

    fn check(&self) -> Result<(), ManifestError> {
        self.version
            .parse::<Version>()
            .map_err(|_| ManifestError::Version(self.version.clone()))?;
        for file in &self.files {
            if !is_bare_file_name(&file.name) {
                return Err(ManifestError::UnsafeName(file.name.clone()));
            }
            if !file.url.starts_with("https://") {
                return Err(ManifestError::InsecureUrl(file.url.clone()));
            }
            if file.sha256.len() != SHA256_HEX_LEN || hex::decode(&file.sha256).is_none() {
                return Err(ManifestError::BadHash(file.name.clone()));
            }
        }
        Ok(())
    }
}

/// A plain file name: no folders, drive, `..` or characters Windows
/// refuses (so it can only land next to wshare.exe).
fn is_bare_file_name(name: &str) -> bool {
    const FORBIDDEN: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];
    !name.is_empty()
        && !name.ends_with(['.', ' '])
        && name
            .chars()
            .all(|c| !c.is_control() && !FORBIDDEN.contains(&c))
}

/// The SHA-256 of `bytes`, as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(digest(&SHA256, bytes).as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, url: &str) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            url: url.to_string(),
            sha256: sha256_hex(b"exe"),
            size: 3,
        }
    }

    fn manifest(files: Vec<FileEntry>) -> Manifest {
        Manifest {
            version: "0.2.0".to_string(),
            notes: "wshare v0.2.0".to_string(),
            pub_date: "2026-11-17T12:00:00Z".to_string(),
            files,
        }
    }

    #[test]
    fn round_trips_through_json() {
        let m = manifest(vec![entry(
            "wshare.exe",
            "https://github.com/o/r/wshare.exe",
        )]);
        assert_eq!(Manifest::parse(m.to_json().as_bytes()).unwrap(), m);
        assert!(m.file("WSHARE.EXE").is_some());
        assert!(m.file("wshare-cli.exe").is_none());
    }

    #[test]
    fn names_outside_the_folder_are_refused() {
        for name in [
            "..",
            "..\\wshare.exe",
            "sub/wshare.exe",
            "C:wshare.exe",
            "",
            "a.exe.",
        ] {
            let m = manifest(vec![entry(name, "https://x/y")]);
            assert_eq!(
                Manifest::parse(m.to_json().as_bytes()),
                Err(ManifestError::UnsafeName(name.to_string())),
                "{name:?}"
            );
        }
    }

    #[test]
    fn plain_http_and_bad_hashes_are_refused() {
        let m = manifest(vec![entry("wshare.exe", "http://x/y")]);
        assert!(matches!(
            Manifest::parse(m.to_json().as_bytes()),
            Err(ManifestError::InsecureUrl(_))
        ));
        let mut bad = entry("wshare.exe", "https://x/y");
        bad.sha256 = "abc".to_string();
        assert!(matches!(
            Manifest::parse(manifest(vec![bad]).to_json().as_bytes()),
            Err(ManifestError::BadHash(_))
        ));
    }

    #[test]
    fn a_bad_version_or_json_is_refused() {
        let mut m = manifest(Vec::new());
        m.version = "nuova".to_string();
        assert!(matches!(
            Manifest::parse(m.to_json().as_bytes()),
            Err(ManifestError::Version(_))
        ));
        assert!(matches!(Manifest::parse(b"{"), Err(ManifestError::Json(_))));
    }

    #[test]
    fn sha256_matches_a_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
