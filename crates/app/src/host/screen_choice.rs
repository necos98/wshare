//! What a viewer chose to watch (the monitor and the quality) and the
//! stream settings that follow from it. Pure: the session keeps the choice,
//! the streaming thread turns it into capture and encoder settings.

use protocol::session::{MAX_MONITORS, Quality};

use crate::options::StreamOptions;

/// Highest bitrate (Mbit/s) and frame rate of a quality level other than
/// [`Quality::Automatic`], which keeps the host's own settings. Never above
/// the host's: a viewer may ask for less than the host allows, not more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Preset {
    mbps: u32,
    fps: u32,
}

const HIGH: Preset = Preset { mbps: 40, fps: 60 };
const MEDIUM: Preset = Preset { mbps: 15, fps: 30 };
const LOW: Preset = Preset { mbps: 5, fps: 30 };

fn preset(quality: Quality) -> Option<Preset> {
    match quality {
        Quality::Automatic => None,
        Quality::High => Some(HIGH),
        Quality::Medium => Some(MEDIUM),
        Quality::Low => Some(LOW),
    }
}

/// The monitor (index among the host's monitors) and the quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScreenChoice {
    pub(super) monitor: usize,
    pub(super) quality: Quality,
}

impl ScreenChoice {
    /// The host's settings, `base`, adapted to this choice: its monitor,
    /// and the quality's bitrate ceiling and frame rate, each capped by the
    /// host's own maximum. The codec stays.
    pub(super) fn options(&self, base: &StreamOptions) -> StreamOptions {
        let (mbps, fps) = preset(self.quality).map_or((base.mbps, base.fps), |p| {
            (p.mbps.min(base.mbps), p.fps.min(base.fps))
        });
        StreamOptions {
            monitor: self.monitor,
            fps,
            mbps,
            codec: base.codec,
        }
    }
}

/// The monitor a new session starts on: the one a viewer chose last
/// (`remembered`) while it is still there, else the one in the host's
/// settings (`configured`).
pub(super) fn start_monitor(remembered: usize, configured: usize, available: usize) -> usize {
    if remembered < available {
        remembered
    } else {
        configured
    }
}

/// The choice a session works towards: the latest the viewer asked for,
/// corrected by what the streaming thread actually managed to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SessionChoice {
    wanted: ScreenChoice,
}

impl SessionChoice {
    /// Starts on `monitor` at the host's own quality.
    pub(super) fn new(monitor: usize) -> Self {
        Self {
            wanted: ScreenChoice {
                monitor,
                quality: Quality::Automatic,
            },
        }
    }

    pub(super) fn current(&self) -> ScreenChoice {
        self.wanted
    }

    /// The viewer asked for the monitor at `index` of the list it was
    /// sent, while the host has `available` monitors. Returns the choice to
    /// stream when it is a change; `None` for the monitor already chosen or
    /// one that does not exist (anymore), which a viewer cannot make up.
    pub(super) fn select_monitor(&mut self, index: u32, available: usize) -> Option<ScreenChoice> {
        let index = usize::try_from(index).ok()?;
        if index >= available.min(MAX_MONITORS) {
            return None;
        }
        self.change(ScreenChoice {
            monitor: index,
            ..self.wanted
        })
    }

    /// The viewer asked for `quality`; the choice to stream if it changed.
    pub(super) fn set_quality(&mut self, quality: Quality) -> Option<ScreenChoice> {
        self.change(ScreenChoice {
            quality,
            ..self.wanted
        })
    }

    /// The streaming thread now streams `running` (what was asked, or the
    /// previous choice when that could not start).
    pub(super) fn confirm(&mut self, running: ScreenChoice) {
        self.wanted = running;
    }

    fn change(&mut self, next: ScreenChoice) -> Option<ScreenChoice> {
        (next != self.wanted).then(|| {
            self.wanted = next;
            next
        })
    }
}

#[cfg(test)]
mod tests {
    use codec::Codec;

    use super::*;

    fn base() -> StreamOptions {
        StreamOptions {
            monitor: 0,
            fps: 60,
            mbps: 25,
            codec: Codec::Hevc,
        }
    }

    #[test]
    fn sessions_start_on_the_last_choice_while_that_monitor_exists() {
        assert_eq!(start_monitor(1, 0, 2), 1);
        assert_eq!(start_monitor(1, 0, 1), 0, "unplugged: back to the settings");
    }

    #[test]
    fn automatic_keeps_the_host_settings_on_the_chosen_monitor() {
        let choice = ScreenChoice {
            monitor: 2,
            quality: Quality::Automatic,
        };
        assert_eq!(
            choice.options(&base()),
            StreamOptions {
                monitor: 2,
                ..base()
            }
        );
    }

    #[test]
    fn each_level_sets_the_bitrate_ceiling_and_the_frame_rate() {
        let generous = StreamOptions {
            mbps: 100,
            ..base()
        };
        for (quality, mbps, fps) in [
            (Quality::High, 40, 60),
            (Quality::Medium, 15, 30),
            (Quality::Low, 5, 30),
        ] {
            let options = ScreenChoice {
                monitor: 1,
                quality,
            }
            .options(&generous);
            assert_eq!((options.mbps, options.fps), (mbps, fps), "{quality:?}");
            assert_eq!(options.codec, Codec::Hevc, "the codec stays");
            assert_eq!(options.monitor, 1);
        }
    }

    #[test]
    fn no_level_goes_above_the_host_maximums() {
        let host = StreamOptions {
            mbps: 8,
            fps: 30,
            ..base()
        };
        for (quality, mbps, fps) in [
            (Quality::Automatic, 8, 30),
            (Quality::High, 8, 30),
            (Quality::Medium, 8, 30),
            (Quality::Low, 5, 30),
        ] {
            let options = ScreenChoice {
                monitor: 0,
                quality,
            }
            .options(&host);
            assert_eq!((options.mbps, options.fps), (mbps, fps), "{quality:?}");
        }
    }

    #[test]
    fn lower_levels_never_ask_for_more() {
        let levels: Vec<Preset> = [Quality::High, Quality::Medium, Quality::Low]
            .into_iter()
            .filter_map(preset)
            .collect();
        for pair in levels.windows(2) {
            assert!(pair[1].mbps < pair[0].mbps && pair[1].fps <= pair[0].fps);
        }
    }

    #[test]
    fn a_new_monitor_is_a_change_the_same_one_is_not() {
        let mut choice = SessionChoice::new(0);
        assert_eq!(choice.select_monitor(0, 2), None);
        let next = choice.select_monitor(1, 2).unwrap();
        assert_eq!(next.monitor, 1);
        assert_eq!(choice.current(), next);
        assert_eq!(choice.select_monitor(1, 2), None, "already asked for");
    }

    #[test]
    fn monitors_that_do_not_exist_are_refused() {
        let mut choice = SessionChoice::new(0);
        assert_eq!(choice.select_monitor(2, 2), None);
        assert_eq!(choice.select_monitor(u32::MAX, 2), None);
        let listed_at_most = MAX_MONITORS as u32;
        assert_eq!(choice.select_monitor(listed_at_most, 100), None);
        assert_eq!(choice.current().monitor, 0);
    }

    #[test]
    fn quality_changes_keep_the_monitor() {
        let mut choice = SessionChoice::new(1);
        assert_eq!(choice.set_quality(Quality::Automatic), None);
        let next = choice.set_quality(Quality::Low).unwrap();
        assert_eq!(
            next,
            ScreenChoice {
                monitor: 1,
                quality: Quality::Low
            }
        );
    }

    #[test]
    fn what_the_thread_could_start_wins_over_what_was_asked() {
        let mut choice = SessionChoice::new(0);
        choice.select_monitor(1, 2);
        let previous = ScreenChoice {
            monitor: 0,
            quality: Quality::Automatic,
        };
        choice.confirm(previous);
        assert_eq!(choice.current(), previous);
        // Asking for the monitor again is a change again.
        assert!(choice.select_monitor(1, 2).is_some());
    }
}
