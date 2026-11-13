//! Where the virtual desktop is. It changes at runtime (monitor plugged,
//! resolution or arrangement changed), so it is queried for every move:
//! `GetSystemMetrics` reads cached per-session values and costs next to
//! nothing.

use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SYSTEM_METRICS_INDEX,
};

use crate::geometry::ScreenArea;

/// Source of the virtual-desktop rectangle.
pub trait DesktopMetrics {
    fn virtual_desktop(&self) -> ScreenArea;
}

/// A fixed layout (tests, or a caller that tracks the layout itself).
impl DesktopMetrics for ScreenArea {
    fn virtual_desktop(&self) -> ScreenArea {
        *self
    }
}

/// The live layout from `GetSystemMetrics`.
///
/// The values are in the calling thread's DPI awareness: the process must
/// be per-monitor DPI aware, like the capture side, so they are physical
/// pixels matching the monitor rectangle given to [`crate::RemoteInput`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemMetrics;

impl DesktopMetrics for SystemMetrics {
    fn virtual_desktop(&self) -> ScreenArea {
        let metric = |index: SYSTEM_METRICS_INDEX| {
            // SAFETY: plain value query with a valid index; no pointers.
            unsafe { GetSystemMetrics(index) }
        };
        ScreenArea {
            left: metric(SM_XVIRTUALSCREEN),
            top: metric(SM_YVIRTUALSCREEN),
            // 0 on failure: an empty area, which makes moves be ignored.
            width: u32::try_from(metric(SM_CXVIRTUALSCREEN)).unwrap_or(0),
            height: u32::try_from(metric(SM_CYVIRTUALSCREEN)).unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_metrics_reads_without_side_effects() {
        // Read-only query; the desktop of a test runner is non-empty.
        let d = SystemMetrics.virtual_desktop();
        assert!(!d.is_empty());
    }
}
