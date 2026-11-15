//! Settings shared by the command line, the window and the host service.

use codec::Codec;

pub const DEFAULT_PORT: u16 = 7420;

/// Encoding settings shared by `host` and `bench`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOptions {
    pub monitor: usize,
    pub fps: u32,
    pub mbps: u32,
    pub codec: Codec,
}

impl StreamOptions {
    pub fn encoder_config(&self, width: u32, height: u32) -> codec::EncoderConfig {
        codec::EncoderConfig {
            codec: self.codec,
            width,
            height,
            fps: self.fps,
            bitrate_bps: self.mbps.saturating_mul(1_000_000),
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
}
