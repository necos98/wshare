//! `settings.toml` in the data folder: readable and hand-editable, replaced
//! atomically, and never fatal when damaged.

use std::path::Path;

use anyhow::Result;

use crate::text;
use crate::trust::store;

use super::settings::Settings;

/// The settings in `path`. A missing file gives the defaults; so does a
/// damaged or unreadable one, with the reason as the warning. Numbers out
/// of range are brought back into it.
pub fn load_from(path: &Path) -> (Settings, Option<String>) {
    let loaded = store::load::<Settings>(path);
    (loaded.value.sanitized(), loaded.warning)
}

pub fn save_to(path: &Path, settings: &Settings) -> Result<()> {
    store::save(path, text::SETTINGS_FILE_HEADER, settings)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::settings::CodecChoice;
    use super::*;

    fn temp_path() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        (dir, path)
    }

    #[test]
    fn a_missing_file_gives_the_defaults_without_a_warning() {
        let (_dir, path) = temp_path();
        assert_eq!(load_from(&path), (Settings::default(), None));
    }

    #[test]
    fn saved_settings_come_back_unchanged() {
        let (_dir, path) = temp_path();
        let settings = Settings {
            monitor: 2,
            fps: 30,
            max_mbps: 12,
            codec: CodecChoice::Hevc,
            port: 7500,
            allow_input: false,
            viewer_fullscreen: true,
            viewer_stats: true,
            viewer_view_only: true,
            start_with_windows: true,
            start_minimized: true,
            share_on_start: true,
            notifications: false,
            connection_indicator: false,
        };
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path), (settings, None));
    }

    #[test]
    fn the_file_is_readable_text() {
        let (_dir, path) = temp_path();
        save_to(&path, &Settings::default()).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("fps = 60"));
        assert!(text.contains("codec = \"h264\""));
    }

    #[test]
    fn missing_fields_take_their_defaults_and_unknown_ones_are_ignored() {
        let (_dir, path) = temp_path();
        fs::write(&path, "fps = 30\nfrom_the_future = true\n").unwrap();
        let (settings, warning) = load_from(&path);
        assert_eq!(
            settings,
            Settings {
                fps: 30,
                ..Settings::default()
            }
        );
        assert_eq!(warning, None);
    }

    #[test]
    fn a_damaged_file_gives_the_defaults_and_a_warning() {
        for garbage in ["fps = [", "fps = \"fast\"", "codec = \"vp9\"", "\u{0}\u{1}"] {
            let (_dir, path) = temp_path();
            fs::write(&path, garbage).unwrap();
            let (settings, warning) = load_from(&path);
            assert_eq!(settings, Settings::default(), "{garbage:?}");
            assert!(warning.is_some(), "{garbage:?}");
        }
    }

    #[test]
    fn a_non_utf8_file_gives_the_defaults_and_a_warning() {
        let (_dir, path) = temp_path();
        fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();
        let (settings, warning) = load_from(&path);
        assert_eq!(settings, Settings::default());
        assert!(warning.is_some());
    }

    #[test]
    fn numbers_out_of_range_are_brought_back_into_it() {
        let (_dir, path) = temp_path();
        fs::write(&path, "fps = 0\nmax_mbps = 99999\nport = 0\n").unwrap();
        let (settings, warning) = load_from(&path);
        assert_eq!(
            (settings.fps, settings.max_mbps, settings.port),
            (1, 500, 1)
        );
        assert_eq!(warning, None);
    }

    #[test]
    fn saving_over_a_damaged_file_repairs_it() {
        let (_dir, path) = temp_path();
        fs::write(&path, "fps = [").unwrap();
        save_to(&path, &Settings::default()).unwrap();
        assert_eq!(load_from(&path), (Settings::default(), None));
    }
}
