//! Settings shared by the command line, the window and the host service.

use std::path::PathBuf;

use codec::Codec;

use crate::access_mode::AccessMode;

pub const DEFAULT_PORT: u16 = 7420;

const BITS_PER_MEGABIT: u32 = 1_000_000;

/// Encoding settings shared by `host` and `bench`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOptions {
    pub monitor: usize,
    pub fps: u32,
    /// The bitrate the stream starts at and never exceeds: the host lowers
    /// it while the network is congested (`bench` keeps it fixed).
    pub mbps: u32,
    pub codec: Codec,
}

impl StreamOptions {
    /// The maximum bitrate in bit/s (`mbps` saturates at `u32::MAX`).
    pub fn bitrate_bps(&self) -> u32 {
        self.mbps.saturating_mul(BITS_PER_MEGABIT)
    }

    pub fn encoder_config(&self, width: u32, height: u32) -> codec::EncoderConfig {
        codec::EncoderConfig {
            codec: self.codec,
            width,
            height,
            fps: self.fps,
            bitrate_bps: self.bitrate_bps(),
        }
    }
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            monitor: 0,
            fps: 60,
            mbps: 20,
            codec: Codec::H264,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostOptions {
    pub stream: StreamOptions,
    pub port: u16,
    /// Fixed PIN; a random one is generated when absent.
    pub pin: Option<String>,
    /// Refuse remote mouse/keyboard: viewers can only watch.
    pub view_only: bool,
    /// Share copied text with the viewer (and accept the viewer's, if
    /// remote control is allowed).
    pub share_clipboard: bool,
    /// How an unknown viewer is let in. `wshare host` has nobody to ask,
    /// so it always uses [`AccessMode::PinOnly`].
    pub access: AccessMode,
    /// Accept the files the viewer drops or sends (they also need remote
    /// control).
    pub receive_files: bool,
    /// Let the viewer hear this PC's sound (it can still mute it).
    pub share_audio: bool,
    /// Where the viewer's files land; `None`: this user's
    /// `Downloads\wshare` (see `paths::default_downloads`).
    pub downloads: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_follows_megabits_and_saturates() {
        let mut stream = StreamOptions::default();
        assert_eq!(stream.bitrate_bps(), 20_000_000);
        assert_eq!(stream.encoder_config(64, 64).bitrate_bps, 20_000_000);
        stream.mbps = u32::MAX;
        assert_eq!(stream.bitrate_bps(), u32::MAX);
    }
}
