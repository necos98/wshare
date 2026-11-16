//! The settings of the host that the unattended-access service applies,
//! copied from the window's settings at every install or update (the
//! service cannot read the user's settings, and must not depend on them).
//!
//! The others do not apply with nobody in front of the PC: remote control
//! is always allowed, the access mode is replaced by "authorized devices
//! only" (plus the password) and files are never received (they would land
//! as SYSTEM in the profile of nobody).

use anyhow::{Context, Result, bail};

use crate::cli::{codec_name, stream_flag};
use crate::options::{DEFAULT_PORT, StreamOptions};

const PORT_FLAG: &str = "--port";
const NO_CLIPBOARD_FLAG: &str = "--no-clipboard";
const NO_AUDIO_FLAG: &str = "--no-audio";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSettings {
    /// The UDP port viewers connect to.
    pub port: u16,
    /// Monitor, frame rate, maximum bitrate and codec.
    pub stream: StreamOptions,
    /// Copied text and files are shared with the viewer, both ways.
    pub share_clipboard: bool,
    /// The viewer hears this PC's sound.
    pub share_audio: bool,
}

/// What `wshare service install` sets up without options, and what a
/// `service.toml` written before these settings existed is read as: what
/// the service has always done.
impl Default for ServiceSettings {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            stream: StreamOptions::default(),
            share_clipboard: true,
            share_audio: true,
        }
    }
}

impl ServiceSettings {
    /// The `wshare service install` arguments that give these settings
    /// (read back by [`Self::parse_flag`]).
    pub fn install_args(&self) -> Vec<String> {
        let stream = &self.stream;
        let mut args = [
            (PORT_FLAG, self.port.to_string()),
            ("--monitor", stream.monitor.to_string()),
            ("--fps", stream.fps.to_string()),
            ("--mbps", stream.mbps.to_string()),
            ("--codec", codec_name(stream.codec).to_string()),
        ]
        .into_iter()
        .flat_map(|(flag, value)| [flag.to_string(), value])
        .collect::<Vec<_>>();
        if !self.share_clipboard {
            args.push(NO_CLIPBOARD_FLAG.to_string());
        }
        if !self.share_audio {
            args.push(NO_AUDIO_FLAG.to_string());
        }
        args
    }

    /// Handles an `install` flag that belongs to these settings; returns
    /// false if it isn't one.
    pub(super) fn parse_flag(
        &mut self,
        flag: &str,
        value: &mut dyn FnMut() -> Result<String>,
    ) -> Result<bool> {
        if stream_flag(&mut self.stream, flag, value)? {
            return Ok(true);
        }
        match flag {
            PORT_FLAG => {
                self.port = value()?.parse().context(PORT_FLAG)?;
                if self.port == 0 {
                    bail!("{PORT_FLAG} must be between 1 and 65535");
                }
            }
            NO_CLIPBOARD_FLAG => self.share_clipboard = false,
            NO_AUDIO_FLAG => self.share_audio = false,
            _ => return Ok(false),
        }
        Ok(true)
    }
}
