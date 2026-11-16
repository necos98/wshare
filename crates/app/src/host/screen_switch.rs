//! A session's side of switching screens: the viewer's requests become
//! choices for the streaming thread, and once the stream started over the
//! session tells the viewer, aims remote input at the new screen and
//! reports it. Pure: the monitors are looked up by the caller.

use capture::MonitorInfo;
use input::ScreenArea;
use protocol::control::{ControlMessage, StreamInfo};
use protocol::session::Quality;

use super::HostEvent;
use super::monitor_list::{area, monitor_list};
use super::screen_choice::{ScreenChoice, SessionChoice};

/// Where remote input goes after the stream started over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputTarget {
    /// Aim at `area`; on `other_monitor`, release what is held first.
    Aim {
        area: ScreenArea,
        other_monitor: bool,
    },
    /// The monitor streamed is not among the host's monitors anymore:
    /// release what is held, as no area is known to aim at.
    Unknown,
}

/// What the session does once the stream started over.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Switched {
    /// For the viewer, in this order: the new stream, then the monitor
    /// list (the choice and resolutions in it may have changed), when the
    /// monitors are known.
    pub(super) messages: Vec<ControlMessage>,
    pub(super) input: InputTarget,
    /// For the front-end: the screen shared now.
    pub(super) event: Option<HostEvent>,
}

/// The screens of one session: what the viewer asked for and the monitor
/// streamed.
pub(super) struct SessionScreens {
    choice: SessionChoice,
    /// The monitor remote input aims at.
    streamed: usize,
}

impl SessionScreens {
    pub(super) fn new(monitor: usize) -> Self {
        Self {
            choice: SessionChoice::new(monitor),
            streamed: monitor,
        }
    }

    /// The choice the stream starts with.
    pub(super) fn current(&self) -> ScreenChoice {
        self.choice.current()
    }

    /// The list the viewer gets after Welcome.
    pub(super) fn list(&self, monitors: &[MonitorInfo]) -> ControlMessage {
        monitor_list(monitors, self.streamed)
    }

    /// The viewer asked for the monitor at `index`, while the host has
    /// `monitors`. The choice to stream, if it is a new one.
    pub(super) fn select_monitor(
        &mut self,
        index: u32,
        monitors: &[MonitorInfo],
    ) -> Option<ScreenChoice> {
        self.choice.select_monitor(index, monitors.len())
    }

    /// The viewer asked for `quality`; the choice to stream if new.
    pub(super) fn set_quality(&mut self, quality: Quality) -> Option<ScreenChoice> {
        self.choice.set_quality(quality)
    }

    /// The streaming thread started the stream over for `running`, whose
    /// video is `info`; `monitors` are the host's monitors now.
    pub(super) fn restarted(
        &mut self,
        running: ScreenChoice,
        info: StreamInfo,
        monitors: &[MonitorInfo],
    ) -> Switched {
        self.choice.confirm(running);
        let other_monitor = running.monitor != self.streamed;
        self.streamed = running.monitor;
        let shared = monitors.get(running.monitor);
        let input = shared.map_or(InputTarget::Unknown, |monitor| InputTarget::Aim {
            area: area(monitor),
            other_monitor,
        });
        let event = shared.map(|monitor| HostEvent::ScreenChanged {
            monitor: monitor.name.clone(),
            width: info.width,
            height: info.height,
        });
        let mut messages = vec![ControlMessage::StreamChanged(info)];
        // No list known (Windows could not tell): the viewer keeps its own.
        if !monitors.is_empty() {
            messages.push(monitor_list(monitors, running.monitor));
        }
        Switched {
            messages,
            input,
            event,
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::control::VideoCodec;

    use super::*;

    fn monitor(index: usize, left: i32, width: u32) -> MonitorInfo {
        MonitorInfo {
            index,
            name: format!("DISPLAY{}", index + 1),
            adapter_name: "GPU".into(),
            left,
            top: 0,
            width,
            height: 1080,
            primary: left == 0,
        }
    }

    fn two_monitors() -> Vec<MonitorInfo> {
        vec![monitor(0, 0, 1920), monitor(1, 1920, 2560)]
    }

    fn info(width: u32) -> StreamInfo {
        StreamInfo {
            codec: VideoCodec::H264,
            width,
            height: 1080,
            fps: 60,
            encoder: "E".into(),
        }
    }

    fn on(monitor: usize) -> ScreenChoice {
        ScreenChoice {
            monitor,
            quality: Quality::Automatic,
        }
    }

    #[test]
    fn the_first_list_marks_the_monitor_shared() {
        let screens = SessionScreens::new(1);
        let ControlMessage::MonitorList { monitors, current } = screens.list(&two_monitors())
        else {
            panic!("not a list")
        };
        assert_eq!((monitors.len(), current), (2, 1));
    }

    #[test]
    fn another_monitor_tells_the_viewer_moves_input_and_reports_it() {
        let mut screens = SessionScreens::new(0);
        let choice = screens.select_monitor(1, &two_monitors()).unwrap();
        let switched = screens.restarted(choice, info(2560), &two_monitors());
        assert_eq!(
            switched.messages[0],
            ControlMessage::StreamChanged(info(2560))
        );
        assert!(matches!(
            switched.messages[1],
            ControlMessage::MonitorList { current: 1, .. }
        ));
        assert_eq!(
            switched.input,
            InputTarget::Aim {
                area: ScreenArea::new(1920, 0, 2560, 1080),
                other_monitor: true
            }
        );
        assert_eq!(
            switched.event,
            Some(HostEvent::ScreenChanged {
                monitor: "DISPLAY2".into(),
                width: 2560,
                height: 1080
            })
        );
    }

    #[test]
    fn a_new_resolution_of_the_same_monitor_keeps_what_is_held() {
        let mut screens = SessionScreens::new(0);
        let resized = vec![monitor(0, 0, 1280), monitor(1, 1280, 2560)];
        let switched = screens.restarted(on(0), info(1280), &resized);
        assert_eq!(
            switched.input,
            InputTarget::Aim {
                area: ScreenArea::new(0, 0, 1280, 1080),
                other_monitor: false
            }
        );
    }

    #[test]
    fn a_fallback_to_the_previous_monitor_is_what_the_viewer_sees() {
        let mut screens = SessionScreens::new(0);
        screens.select_monitor(1, &two_monitors());
        let switched = screens.restarted(on(0), info(1920), &two_monitors());
        assert!(matches!(
            switched.messages[1],
            ControlMessage::MonitorList { current: 0, .. }
        ));
        assert_eq!(screens.current(), on(0));
        assert!(screens.select_monitor(1, &two_monitors()).is_some());
    }

    #[test]
    fn a_monitor_gone_from_the_list_releases_input() {
        let mut screens = SessionScreens::new(0);
        let switched = screens.restarted(on(1), info(2560), &two_monitors()[..1]);
        assert_eq!(switched.input, InputTarget::Unknown);
        assert_eq!(switched.event, None);
    }

    #[test]
    fn without_known_monitors_the_viewer_keeps_its_list() {
        let mut screens = SessionScreens::new(0);
        let switched = screens.restarted(on(0), info(1920), &[]);
        assert_eq!(
            switched.messages,
            [ControlMessage::StreamChanged(info(1920))]
        );
        assert_eq!(switched.input, InputTarget::Unknown);
    }

    #[test]
    fn requests_for_what_already_streams_start_nothing() {
        let mut screens = SessionScreens::new(0);
        assert_eq!(screens.select_monitor(0, &two_monitors()), None);
        assert_eq!(screens.select_monitor(5, &two_monitors()), None);
        assert_eq!(screens.set_quality(Quality::Automatic), None);
        assert_eq!(
            screens.set_quality(Quality::High),
            Some(ScreenChoice {
                monitor: 0,
                quality: Quality::High
            })
        );
    }
}
