//! [`SettingsBackend`] backed by the data folder, the Windows registry and
//! the monitors of this PC.

use anyhow::{Context, Result};

use crate::paths;
use crate::text;

use super::autostart;
use super::ports::{MonitorChoice, SettingsBackend};
use super::settings::Settings;
use super::settings_file;

const SETTINGS_FILE: &str = "settings.toml";

pub struct SystemSettings;

impl SystemSettings {
    fn path() -> Result<std::path::PathBuf> {
        Ok(paths::config_dir()?.join(SETTINGS_FILE))
    }
}

impl SettingsBackend for SystemSettings {
    fn load(&self) -> (Settings, Option<String>) {
        match Self::path() {
            Ok(path) => settings_file::load_from(&path),
            Err(e) => (Settings::default(), Some(format!("{e:#}"))),
        }
    }

    fn save(&self, settings: &Settings) -> Result<()> {
        settings_file::save_to(&Self::path()?, settings)
    }

    fn set_autostart(&self, enabled: bool, minimized: bool) -> Result<()> {
        if !enabled {
            return autostart::apply(None);
        }
        let exe = std::env::current_exe().context("locating wshare.exe")?;
        autostart::apply(Some(&autostart::run_command(&exe, minimized)))
    }

    fn monitors(&self) -> Result<Vec<MonitorChoice>> {
        Ok(capture::list_monitors()?
            .into_iter()
            .map(|m| MonitorChoice {
                index: m.index,
                label: text::monitor_label(m.index, &m.name, m.width, m.height),
            })
            .collect())
    }
}
