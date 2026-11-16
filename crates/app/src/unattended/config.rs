//! The settings of the unattended-access service, written by `install` in
//! the service's protected folder and read by the agent at every start.
//!
//! Unlike the window's settings, a damaged file is an error, not a reason
//! to fall back on defaults: the service must not run with a password
//! silently dropped.

use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::password::PasswordHash;

const HEADER: &str = "# wshare: unattended access. Written by `wshare service install`.\n";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    /// The UDP port viewers connect to.
    pub port: u16,
    /// Asked of every viewer (all of them authorized devices); `None`: the
    /// authorized certificate is enough.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<PasswordHash>,
}

impl ServiceConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("reading {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = format!("{HEADER}{}", toml::to_string(self)?);
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }

    fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        ensure!(config.port != 0, "the port must be between 1 and 65535");
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash() -> String {
        format!(
            "pbkdf2-sha256$600000${}${}",
            "ab".repeat(16),
            "cd".repeat(32)
        )
    }

    #[test]
    fn roundtrips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.toml");
        for config in [
            ServiceConfig {
                port: 7420,
                password: None,
            },
            ServiceConfig {
                port: 9000,
                password: Some(hash().parse().unwrap()),
            },
        ] {
            config.save(&path).unwrap();
            assert_eq!(ServiceConfig::load(&path).unwrap(), config);
        }
    }

    #[test]
    fn the_password_is_written_only_as_its_hash() {
        let config = ServiceConfig {
            port: 7420,
            password: Some(hash().parse().unwrap()),
        };
        let text = toml::to_string(&config).unwrap();
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
        ] {
            assert!(ServiceConfig::parse(text).is_err(), "{text:?}");
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(ServiceConfig::load(&dir.path().join("missing.toml")).is_err());
    }
}
