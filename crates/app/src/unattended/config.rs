//! The settings of the unattended-access service, written by `install` in
//! the service's protected folder and read by the agent at every start.
//!
//! Unlike the window's settings, a damaged file is an error, not a reason
//! to fall back on defaults: the service must not run with a password
//! silently dropped. Only the settings added after the first version may
//! be missing (a file written by an older wshare): they take what the
//! service did before them (see [`ServiceSettings::default`]).

use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::password::PasswordHash;
use super::service_settings::ServiceSettings;
use crate::cli::{codec_name, parse_codec};
use crate::options::StreamOptions;

const HEADER: &str = "# wshare: unattended access. Written by `wshare service install`.\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    /// The settings of the host the agent applies.
    pub settings: ServiceSettings,
    /// Asked of every viewer (all of them authorized devices); `None`: the
    /// authorized certificate is enough.
    pub password: Option<PasswordHash>,
}

impl ServiceConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("reading {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = format!("{HEADER}{}", toml::to_string(&StoredConfig::from(self))?);
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }

    fn parse(text: &str) -> Result<Self> {
        toml::from_str::<StoredConfig>(text)?.try_into()
    }
}

/// `service.toml` as written: flat, every setting named as in the window's
/// `settings.toml`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredConfig {
    port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<PasswordHash>,
    #[serde(default = "defaults::monitor")]
    monitor: usize,
    #[serde(default = "defaults::fps")]
    fps: u32,
    #[serde(default = "defaults::max_mbps")]
    max_mbps: u32,
    /// `h264` or `hevc`, as `--codec` takes it.
    #[serde(default = "defaults::codec")]
    codec: String,
    #[serde(default = "defaults::share_clipboard")]
    share_clipboard: bool,
    #[serde(default = "defaults::share_audio")]
    share_audio: bool,
}

impl From<&ServiceConfig> for StoredConfig {
    fn from(config: &ServiceConfig) -> Self {
        let settings = &config.settings;
        Self {
            port: settings.port,
            password: config.password.clone(),
            monitor: settings.stream.monitor,
            fps: settings.stream.fps,
            max_mbps: settings.stream.mbps,
            codec: codec_name(settings.stream.codec).to_string(),
            share_clipboard: settings.share_clipboard,
            share_audio: settings.share_audio,
        }
    }
}

impl TryFrom<StoredConfig> for ServiceConfig {
    type Error = anyhow::Error;

    fn try_from(stored: StoredConfig) -> Result<Self> {
        ensure!(stored.port != 0, "the port must be between 1 and 65535");
        ensure!(stored.fps != 0, "fps must be a positive integer");
        ensure!(stored.max_mbps != 0, "max_mbps must be a positive integer");
        Ok(Self {
            settings: ServiceSettings {
                port: stored.port,
                stream: StreamOptions {
                    monitor: stored.monitor,
                    fps: stored.fps,
                    mbps: stored.max_mbps,
                    codec: parse_codec(&stored.codec)?,
                },
                share_clipboard: stored.share_clipboard,
                share_audio: stored.share_audio,
            },
            password: stored.password,
        })
    }
}

/// The values of the settings a `service.toml` may lack.
mod defaults {
    use super::{ServiceSettings, codec_name};

    fn settings() -> ServiceSettings {
        ServiceSettings::default()
    }

    pub(super) fn monitor() -> usize {
        settings().stream.monitor
    }

    pub(super) fn fps() -> u32 {
        settings().stream.fps
    }

    pub(super) fn max_mbps() -> u32 {
        settings().stream.mbps
    }

    pub(super) fn codec() -> String {
        codec_name(settings().stream.codec).to_string()
    }

    pub(super) fn share_clipboard() -> bool {
        settings().share_clipboard
    }

    pub(super) fn share_audio() -> bool {
        settings().share_audio
    }
}

#[cfg(test)]
mod tests {
    use codec::Codec;

    use super::*;

    fn hash() -> String {
        format!(
            "pbkdf2-sha256$600000${}${}",
            "ab".repeat(16),
            "cd".repeat(32)
        )
    }

    fn private_settings() -> ServiceSettings {
        ServiceSettings {
            port: 9000,
            stream: StreamOptions {
                monitor: 1,
                fps: 30,
                mbps: 8,
                codec: Codec::Hevc,
            },
            share_clipboard: false,
            share_audio: false,
        }
    }

    #[test]
    fn roundtrips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.toml");
        for config in [
            ServiceConfig {
                settings: ServiceSettings::default(),
                password: None,
            },
            ServiceConfig {
                settings: private_settings(),
                password: Some(hash().parse().unwrap()),
            },
        ] {
            config.save(&path).unwrap();
            assert_eq!(ServiceConfig::load(&path).unwrap(), config);
        }
    }

    #[test]
    fn a_turned_off_clipboard_is_written_and_read_back() {
        let config = ServiceConfig {
            settings: private_settings(),
            password: None,
        };
        let text = toml::to_string(&StoredConfig::from(&config)).unwrap();
        assert!(text.contains("share_clipboard = false"), "{text}");
        let read = ServiceConfig::parse(&text).unwrap();
        assert!(!read.settings.share_clipboard);
    }

    #[test]
    fn a_file_written_before_the_sharing_settings_takes_their_defaults() {
        let old = format!("port = 9000\npassword = \"{}\"\n", hash());
        let config = ServiceConfig::parse(&old).unwrap();
        assert_eq!(
            config.settings,
            ServiceSettings {
                port: 9000,
                ..ServiceSettings::default()
            }
        );
        assert_eq!(config.password, Some(hash().parse().unwrap()));
    }

    #[test]
    fn the_password_is_written_only_as_its_hash() {
        let config = ServiceConfig {
            settings: ServiceSettings::default(),
            password: Some(hash().parse().unwrap()),
        };
        let text = toml::to_string(&StoredConfig::from(&config)).unwrap();
        assert!(
            text.contains(&format!("password = \"{}\"", hash())),
            "{text}"
        );
    }

    #[test]
    fn a_damaged_file_is_an_error() {
        for text in [
            "",
            "port = 0",
            "port = 70000",
            "port = 7420\npassword = \"segreto\"",
            "port = 7420\npin = \"123456\"",
            "port = 7420\nfps = 0",
            "port = 7420\nmax_mbps = 0",
            "port = 7420\ncodec = \"vp9\"",
            "port = 7420\nshare_clipboard = \"no\"",
            "port = 7420\nreceive_files = true",
        ] {
            assert!(ServiceConfig::parse(text).is_err(), "{text:?}");
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(ServiceConfig::load(&dir.path().join("missing.toml")).is_err());
    }
}
