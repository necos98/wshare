//! Rate-adaptation payloads: what the viewer reports about the video it
//! received, so the host can fit the encoder's bitrate to the network.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How often the viewer sends a [`ReceptionReport`]: often enough for the
/// host to react to congestion within about a second, rarely enough to
/// cost nothing on the control channel.
pub const RECEPTION_REPORT_INTERVAL: Duration = Duration::from_millis(500);

/// What went wrong with the video since the viewer's previous report.
/// Counts, not totals: the control stream is reliable, so none is lost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceptionReport {
    /// Frames that never arrived whole (some datagram was lost).
    pub frames_lost: u32,
    /// Frames that arrived too late to be shown: a newer one was already
    /// waiting, i.e. video queued up somewhere on the way.
    pub frames_late: u32,
}
