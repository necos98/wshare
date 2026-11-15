//! Small TOML files holding trust decisions: readable, replaced atomically,
//! and never fatal when damaged.
//!
//! A damaged or unreadable file loads as empty with a warning. For the
//! stores built on this that is the safe direction: an empty list of
//! authorized viewers or known hosts only means the PIN is asked again.

use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Far above any real list; protects against reading a huge file into memory.
const MAX_FILE_SIZE: u64 = 1024 * 1024;

/// What [`load`] found.
#[derive(Debug)]
pub struct Loaded<T> {
    pub value: T,
    /// Why the file was ignored (unreadable, too large, not valid TOML).
    pub warning: Option<String>,
}

/// Reads `path`. A missing file is the default value without warning.
pub fn load<T: DeserializeOwned + Default>(path: &Path) -> Loaded<T> {
    match read(path) {
        Ok(value) => Loaded {
            value: value.unwrap_or_default(),
            warning: None,
        },
        Err(e) => Loaded {
            value: T::default(),
            warning: Some(format!("ignoring {}: {e:#}", path.display())),
        },
    }
}

fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let file = match File::open(path) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        opened => opened.with_context(|| format!("opening {}", path.display()))?,
    };
    let mut text = String::new();
    file.take(MAX_FILE_SIZE + 1)
        .read_to_string(&mut text)
        .with_context(|| format!("reading {}", path.display()))?;
    if text.len() as u64 > MAX_FILE_SIZE {
        bail!("the file is larger than {MAX_FILE_SIZE} bytes");
    }
    toml::from_str(&text)
        .context("the file is damaged")
        .map(Some)
}

/// Replaces `path` with `value`, preceded by the `header` comment lines.
/// Written to a temporary file first and renamed over the old one, so a
/// crash leaves either the old or the new file, never half of one.
pub fn save<T: Serialize>(path: &Path, header: &str, value: &T) -> Result<()> {
    let body = toml::to_string_pretty(value).context("serializing")?;
    let mut text = String::with_capacity(header.len() + body.len() + 1);
    for line in header.lines() {
        text.push_str("# ");
        text.push_str(line);
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&body);

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let temporary = temporary_path(path);
    let written = write_synced(&temporary, text.as_bytes())
        .and_then(|()| fs::rename(&temporary, path).context("replacing the old file"));
    if written.is_err() {
        // Best effort: the error that matters is the one returned.
        let _ = fs::remove_file(&temporary);
    }
    written.with_context(|| format!("saving {}", path.display()))
}

/// Next to `path` (same volume, so the rename is atomic) and unique per
/// process, so two wshare processes saving at once do not share it.
fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    path.with_file_name(name)
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Sample {
        names: Vec<String>,
    }

    fn sample() -> Sample {
        Sample {
            names: vec!["Ufficio".into(), "Jacob's PC \u{2014} salotto".into()],
        }
    }

    #[test]
    fn save_then_load_roundtrips_with_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("sample.toml");
        save(&path, "first line\nsecond line", &sample()).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# first line\n# second line\n"), "{text}");
        let loaded = load::<Sample>(&path);
        assert_eq!(loaded.value, sample());
        assert_eq!(loaded.warning, None);
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temporary file left behind");
    }

    #[test]
    fn save_replaces_the_previous_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.toml");
        save(&path, "", &sample()).unwrap();
        save(&path, "", &Sample::default()).unwrap();
        assert_eq!(load::<Sample>(&path).value, Sample::default());
    }

    #[test]
    fn missing_file_is_empty_without_warning() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load::<Sample>(&dir.path().join("absent.toml"));
        assert_eq!(loaded.value, Sample::default());
        assert_eq!(loaded.warning, None);
    }

    #[test]
    fn damaged_files_are_empty_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.toml");
        for content in [
            &b"names = [unterminated"[..],
            b"names = 3",
            b"\xff\xfe garbage",
        ] {
            fs::write(&path, content).unwrap();
            let loaded = load::<Sample>(&path);
            assert_eq!(loaded.value, Sample::default());
            let warning = loaded.warning.expect("a warning");
            assert!(warning.contains("sample.toml"), "{warning}");
        }
    }

    #[test]
    fn oversized_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.toml");
        let padding = "#".repeat(MAX_FILE_SIZE as usize + 1);
        fs::write(&path, padding).unwrap();
        let warning = load::<Sample>(&path).warning.expect("a warning");
        assert!(warning.contains("larger than"), "{warning}");
    }

    #[test]
    fn a_directory_in_the_way_is_a_save_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.toml");
        fs::create_dir(&path).unwrap();
        assert!(save(&path, "", &sample()).is_err());
        let leftovers: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(
            leftovers.len(),
            1,
            "temporary file removed after the failure"
        );
    }
}
