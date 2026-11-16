//! The host's monitors as a session needs them: the list the viewer picks
//! from, and the area of the virtual desktop remote input aims at.

use anyhow::{Context, Result};
use capture::MonitorInfo;
use input::ScreenArea;
use protocol::control::ControlMessage;
use protocol::session::{MAX_MONITORS, MonitorEntry};

/// The `MonitorList` message for `monitors`, the one at `current` being
/// shared. At most [`MAX_MONITORS`] are listed.
pub(super) fn monitor_list(monitors: &[MonitorInfo], current: usize) -> ControlMessage {
    ControlMessage::MonitorList {
        monitors: monitors.iter().take(MAX_MONITORS).map(entry).collect(),
        current: u32::try_from(current).unwrap_or(u32::MAX),
    }
}

fn entry(monitor: &MonitorInfo) -> MonitorEntry {
    MonitorEntry::new(
        capture::display_name(&monitor.name),
        monitor.width,
        monitor.height,
        monitor.primary,
    )
}

/// Where `monitor` lies on the virtual desktop.
pub(super) fn area(monitor: &MonitorInfo) -> ScreenArea {
    ScreenArea::new(monitor.left, monitor.top, monitor.width, monitor.height)
}

/// The monitor at `index` of `monitors`.
pub(super) fn find(monitors: &[MonitorInfo], index: usize) -> Result<&MonitorInfo> {
    monitors
        .get(index)
        .with_context(|| format!("monitor {index} not found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(index: usize, name: &str, left: i32, primary: bool) -> MonitorInfo {
        MonitorInfo {
            index,
            name: name.into(),
            adapter_name: "GPU".into(),
            left,
            top: 0,
            width: 1920,
            height: 1080,
            primary,
        }
    }

    #[test]
    fn the_list_names_each_monitor_without_the_device_prefix() {
        let monitors = [
            monitor(0, r"\\.\DISPLAY1", 0, true),
            monitor(1, "Other", 1920, false),
        ];
        let ControlMessage::MonitorList { monitors, current } = monitor_list(&monitors, 1) else {
            panic!("not a monitor list")
        };
        assert_eq!(current, 1);
        assert_eq!(
            monitors,
            [
                MonitorEntry::new("DISPLAY1", 1920, 1080, true),
                MonitorEntry::new("Other", 1920, 1080, false),
            ]
        );
    }

    #[test]
    fn the_list_is_bounded() {
        let many: Vec<MonitorInfo> = (0..MAX_MONITORS + 5)
            .map(|i| monitor(i, "M", 0, false))
            .collect();
        let ControlMessage::MonitorList { monitors, .. } = monitor_list(&many, 0) else {
            panic!("not a monitor list")
        };
        assert_eq!(monitors.len(), MAX_MONITORS);
    }

    #[test]
    fn input_aims_at_the_monitor_rectangle() {
        let right = monitor(1, "M", 1920, false);
        assert_eq!(area(&right), ScreenArea::new(1920, 0, 1920, 1080));
    }

    #[test]
    fn a_missing_monitor_is_an_error() {
        let monitors = [monitor(0, "M", 0, true)];
        assert_eq!(find(&monitors, 0).unwrap().name, "M");
        assert!(find(&monitors, 1).is_err());
    }
}
