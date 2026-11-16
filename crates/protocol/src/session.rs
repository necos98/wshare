//! What the viewer can choose while a session runs: which of the host's
//! monitors it watches ([`MonitorEntry`]) and the quality of the video
//! ([`Quality`]).

use serde::{Deserialize, Serialize};

/// Most monitors a [`crate::control::ControlMessage::MonitorList`] carries:
/// the host lists no more, the viewer shows no more.
pub const MAX_MONITORS: usize = 16;

/// Longest monitor name in a list, in characters (not bytes).
pub const MAX_MONITOR_NAME: usize = 64;

/// One monitor of the host, as the viewer's "Screen" menu shows it. Its
/// position in the list is what `SelectMonitor` names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorEntry {
    /// The host's name for it (e.g. "DISPLAY2"), at most
    /// [`MAX_MONITOR_NAME`] characters.
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// The host's main monitor (the one with the taskbar).
    pub primary: bool,
}

impl MonitorEntry {
    /// `name` cut to [`MAX_MONITOR_NAME`] characters, so neither side has
    /// to hold or draw an absurdly long one.
    pub fn new(name: &str, width: u32, height: u32, primary: bool) -> Self {
        Self {
            name: bounded_name(name),
            width,
            height,
            primary,
        }
    }
}

/// `name` cut to [`MAX_MONITOR_NAME`] characters (never inside a character).
pub fn bounded_name(name: &str) -> String {
    name.chars().take(MAX_MONITOR_NAME).collect()
}

/// The video quality the viewer asks for. Anything but `Automatic` sets the
/// highest bitrate the host's adaptation may reach and the frame rate; the
/// host decides what each level means.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Quality {
    /// The host's own settings.
    #[default]
    Automatic,
    High,
    Medium,
    Low,
}

impl Quality {
    /// Every level, in the order menus list them.
    pub const ALL: [Quality; 4] = [Self::Automatic, Self::High, Self::Medium, Self::Low];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_names_are_cut_by_characters() {
        let long = "\u{e8}".repeat(MAX_MONITOR_NAME + 10);
        let entry = MonitorEntry::new(&long, 1920, 1080, true);
        assert_eq!(entry.name.chars().count(), MAX_MONITOR_NAME);
        assert_eq!(MonitorEntry::new("DISPLAY1", 1, 1, false).name, "DISPLAY1");
    }

    #[test]
    fn quality_levels_start_with_automatic() {
        assert_eq!(Quality::ALL[0], Quality::default());
        assert_eq!(Quality::ALL.len(), 4);
    }
}
