//! The preferences the user can change in the "Impostazioni" panel. They
//! are saved in `settings.toml` (see `settings_file`) and apply from the
//! next start of the sharing (streaming options) or at once (the rest).
//!
//! Pure data and rules: no I/O here.

use std::path::PathBuf;

use codec::Codec;
use serde::{Deserialize, Serialize};

use crate::access_mode::AccessMode;
use crate::options::{DEFAULT_PORT, HostOptions, StreamOptions};

pub const MIN_FPS: u32 = 1;
pub const MAX_FPS: u32 = 240;
pub const MIN_MBPS: u32 = 1;
pub const MAX_MBPS: u32 = 500;
pub const MIN_PORT: u16 = 1;

/// The codec as written in the settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodecChoice {
    H264,
    Hevc,
}

impl CodecChoice {
    pub const ALL: [Self; 2] = [Self::H264, Self::Hevc];

    pub fn codec(self) -> Codec {
        match self {
            Self::H264 => Codec::H264,
            Self::Hevc => Codec::Hevc,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC (H.265)",
        }
    }
}

/// Missing fields take their default and unknown ones are ignored, so a
/// file written by an older or newer wshare still loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // Sharing.
    pub monitor: usize,
    pub fps: u32,
    /// The maximum bitrate: the host lowers it while the network is
    /// congested (see `host::rate_adaptation`).
    pub max_mbps: u32,
    pub codec: CodecChoice,
    pub port: u16,
    /// Whether viewers may use this PC's mouse and keyboard.
    pub allow_input: bool,
    /// Whether copied text is shared with the connected viewer (both ways;
    /// the viewer's text also needs `allow_input`).
    pub share_clipboard: bool,
    /// How a viewer that is not authorized is let in (see `AccessMode`).
    pub access: AccessMode,
    /// Whether the viewer may drop, send or paste files on this PC (they
    /// also need `allow_input`).
    pub receive_files: bool,
    /// Whether the viewer hears this PC's sound (it can still mute it).
    pub share_audio: bool,
    /// Where received files land, on this PC as host and as viewer; empty:
    /// `Downloads\wshare`.
    pub downloads: String,
    // Viewer windows opened from this PC.
    pub viewer_fullscreen: bool,
    pub viewer_stats: bool,
    pub viewer_view_only: bool,
    // Start-up.
    pub start_with_windows: bool,
    pub start_minimized: bool,
    pub share_on_start: bool,
    // Feedback on this PC.
    pub notifications: bool,
    pub connection_indicator: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let stream = StreamOptions::default();
        Self {
            monitor: stream.monitor,
            fps: stream.fps,
            max_mbps: stream.mbps,
            codec: CodecChoice::H264,
            port: DEFAULT_PORT,
            allow_input: true,
            share_clipboard: true,
            access: AccessMode::PinOnly,
            receive_files: true,
            share_audio: true,
            downloads: String::new(),
            viewer_fullscreen: false,
            viewer_stats: false,
            viewer_view_only: false,
            start_with_windows: false,
            start_minimized: false,
            share_on_start: false,
            notifications: true,
            connection_indicator: true,
        }
    }
}

/// How a viewer window opens: the options `wshare view` has besides the
/// target (see `cli::ViewOptions`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewerDisplay {
    pub view_only: bool,
    pub fullscreen: bool,
    pub stats: bool,
}

impl ViewerDisplay {
    /// The `wshare view` flags that switch these options on.
    pub fn flags(self) -> Vec<&'static str> {
        [
            (self.view_only, "--view-only"),
            (self.fullscreen, "--fullscreen"),
            (self.stats, "--stats"),
        ]
        .into_iter()
        .filter_map(|(on, flag)| on.then_some(flag))
        .collect()
    }
}

impl Settings {
    /// The same settings with every number brought into its valid range
    /// (a hand-edited file may hold anything).
    pub fn sanitized(mut self) -> Self {
        self.fps = self.fps.clamp(MIN_FPS, MAX_FPS);
        self.max_mbps = self.max_mbps.clamp(MIN_MBPS, MAX_MBPS);
        self.port = self.port.max(MIN_PORT);
        self
    }

    /// The folder chosen for received files; `None`: the default one.
    pub fn downloads_folder(&self) -> Option<PathBuf> {
        let folder = self.downloads.trim();
        (!folder.is_empty()).then(|| PathBuf::from(folder))
    }

    /// How the viewer windows opened from now on look.
    pub fn viewer_display(&self) -> ViewerDisplay {
        ViewerDisplay {
            view_only: self.viewer_view_only,
            fullscreen: self.viewer_fullscreen,
            stats: self.viewer_stats,
        }
    }

    pub fn stream_options(&self) -> StreamOptions {
        StreamOptions {
            monitor: self.monitor,
            fps: self.fps,
            mbps: self.max_mbps,
            codec: self.codec.codec(),
        }
    }

    /// What the sharing service is started with. The PIN is always a
    /// random one: viewers that must not type it are authorized devices.
    pub fn host_options(&self) -> HostOptions {
        HostOptions {
            stream: self.stream_options(),
            port: self.port,
            pin: None,
            view_only: !self.allow_input,
            share_clipboard: self.share_clipboard,
            access: self.access,
            receive_files: self.receive_files,
            share_audio: self.share_audio,
            downloads: self.downloads_folder(),
        }
    }

    /// Whether the app opens hidden in the tray, for this start.
    pub fn opens_minimized(&self, requested: bool) -> bool {
        requested || self.start_minimized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_command_line_ones() {
        let settings = Settings::default();
        assert_eq!(settings.stream_options(), StreamOptions::default());
        assert_eq!(settings.port, DEFAULT_PORT);
        assert!(settings.allow_input);
        assert!(settings.share_clipboard);
        assert!(settings.receive_files, "files are accepted by default");
        assert!(settings.share_audio, "the sound is shared by default");
        assert_eq!(settings.downloads_folder(), None);
        assert!(settings.notifications && settings.connection_indicator);
        assert_eq!(
            settings.access,
            AccessMode::PinOnly,
            "the PIN alone by default"
        );
        assert!(!settings.start_with_windows && !settings.start_minimized);
    }

    #[test]
    fn numbers_are_brought_into_range() {
        let wild = Settings {
            fps: 0,
            max_mbps: 100_000,
            port: 0,
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(
            (wild.fps, wild.max_mbps, wild.port),
            (MIN_FPS, MAX_MBPS, MIN_PORT)
        );
        let fine = Settings::default();
        assert_eq!(fine.clone().sanitized(), fine);
    }

    #[test]
    fn host_options_follow_the_settings() {
        let settings = Settings {
            monitor: 1,
            fps: 30,
            max_mbps: 8,
            codec: CodecChoice::Hevc,
            port: 9000,
            allow_input: false,
            share_clipboard: false,
            access: AccessMode::Confirmation,
            ..Settings::default()
        };
        let options = settings.host_options();
        assert_eq!(options.access, AccessMode::Confirmation);
        assert_eq!(options.stream.monitor, 1);
        assert_eq!(options.stream.fps, 30);
        assert_eq!(options.stream.mbps, 8);
        assert_eq!(options.stream.codec, Codec::Hevc);
        assert_eq!(options.port, 9000);
        assert!(options.view_only);
        assert!(!options.share_clipboard);
        assert_eq!(options.pin, None);
        assert!(options.receive_files);
        assert_eq!(options.downloads, None);
    }

    #[test]
    fn sharing_the_sound_follows_its_setting() {
        assert!(Settings::default().host_options().share_audio);
        let settings = Settings {
            share_audio: false,
            ..Settings::default()
        };
        assert!(!settings.host_options().share_audio);
    }

    #[test]
    fn the_download_folder_is_the_one_typed_or_the_default() {
        let mut settings = Settings {
            receive_files: false,
            downloads: "  D:\\Ricevuti  ".into(),
            ..Settings::default()
        };
        let options = settings.host_options();
        assert!(!options.receive_files);
        assert_eq!(options.downloads, Some(PathBuf::from("D:\\Ricevuti")));
        settings.downloads = "   ".into();
        assert_eq!(settings.downloads_folder(), None);
    }

    #[test]
    fn viewer_flags_follow_the_viewing_options() {
        assert!(Settings::default().viewer_display().flags().is_empty());
        let settings = Settings {
            viewer_view_only: true,
            viewer_stats: true,
            ..Settings::default()
        };
        assert_eq!(
            settings.viewer_display().flags(),
            ["--view-only", "--stats"]
        );
        let all = ViewerDisplay {
            view_only: true,
            fullscreen: true,
            stats: true,
        };
        assert_eq!(all.flags(), ["--view-only", "--fullscreen", "--stats"]);
    }
    #[test]
    fn minimized_start_comes_from_the_flag_or_the_setting() {
        let mut settings = Settings::default();
        assert!(!settings.opens_minimized(false));
        assert!(settings.opens_minimized(true));
        settings.start_minimized = true;
        assert!(settings.opens_minimized(false));
    }
}
