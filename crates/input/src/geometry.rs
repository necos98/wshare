//! Coordinate mapping: normalized monitor position -> `SendInput` absolute
//! coordinates (`MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`).
//!
//! With `MOUSEEVENTF_VIRTUALDESK`, 0..=65535 spans the whole virtual desktop
//! and Windows maps a normalized value `v` back to the pixel
//! `left + floor(v * width / 65536)` (the same formula Wine implements).
//! We first pick the target *pixel*, then emit the normalized value just
//! below the centre of that pixel's band, so the round trip lands exactly on it even
//! if Windows rounded instead of truncating. The far edge (x = 1.0) maps to
//! the last pixel, never one past it. Exact for desktops up to 32767 px
//! wide/high (beyond, a band is narrower than one normalized unit).

/// A rectangle in virtual-desktop pixels. `left`/`top` can be negative
/// (monitors left of / above the primary one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScreenArea {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl ScreenArea {
    pub const fn new(left: i32, top: i32, width: u32, height: u32) -> Self {
        Self {
            left,
            top,
            width,
            height,
        }
    }

    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// `SendInput` absolute coordinates, each in 0..=65535 across the virtual
/// desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsolutePoint {
    pub x: i32,
    pub y: i32,
}

/// Largest normalized `SendInput` coordinate.
pub const ABSOLUTE_MAX: i32 = 65535;

/// Maps `(x, y)`, normalized to `monitor` ((0,0) top-left, (1,1)
/// bottom-right), to absolute coordinates on `desktop` (the virtual desktop).
///
/// Values outside [0,1] are clamped onto the monitor. Returns `None` for
/// NaN/infinite input or an empty rectangle: there is no sane place to move.
pub fn to_absolute(
    x: f32,
    y: f32,
    monitor: &ScreenArea,
    desktop: &ScreenArea,
) -> Option<AbsolutePoint> {
    if !x.is_finite() || !y.is_finite() || monitor.is_empty() || desktop.is_empty() {
        return None;
    }
    let px = i64::from(monitor.left) + pixel_offset(x, monitor.width);
    let py = i64::from(monitor.top) + pixel_offset(y, monitor.height);
    Some(AbsolutePoint {
        x: normalize(px, desktop.left, desktop.width),
        y: normalize(py, desktop.top, desktop.height),
    })
}

/// Pixel index along an axis of `len` pixels: [0,1] is split into `len`
/// equal bands and 1.0 belongs to the last one.
fn pixel_offset(n: f32, len: u32) -> i64 {
    let n = f64::from(n.clamp(0.0, 1.0));
    ((n * f64::from(len)).floor() as i64).min(i64::from(len) - 1)
}

/// Normalized value at the centre of `pixel`'s band on a desktop axis.
/// A pixel outside the desktop (stale monitor rectangle after a layout
/// change) is clamped onto its edge.
fn normalize(pixel: i64, origin: i32, len: u32) -> i32 {
    let len = i64::from(len);
    let p = (pixel - i64::from(origin)).clamp(0, len - 1);
    // Just below the centre so a rounding inverse cannot hit p + 1.
    let v = ((2 * p + 1) * 65536 - 1) / (2 * len);
    v.min(i64::from(ABSOLUTE_MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows' inverse mapping (absolute -> pixel) on one axis.
    fn to_pixel(v: i32, origin: i32, len: u32) -> i64 {
        i64::from(origin) + ((i64::from(v) * i64::from(len)) >> 16)
    }

    fn landing(x: f32, y: f32, monitor: ScreenArea, desktop: ScreenArea) -> (i64, i64) {
        let p = to_absolute(x, y, &monitor, &desktop).expect("valid input");
        assert!((0..=ABSOLUTE_MAX).contains(&p.x) && (0..=ABSOLUTE_MAX).contains(&p.y));
        (
            to_pixel(p.x, desktop.left, desktop.width),
            to_pixel(p.y, desktop.top, desktop.height),
        )
    }

    #[test]
    fn every_pixel_round_trips_for_many_widths() {
        for len in [
            1u32, 2, 3, 7, 640, 1080, 1366, 1920, 2560, 3840, 7680, 11520,
        ] {
            for p in 0..i64::from(len) {
                let v = normalize(p, 0, len);
                assert_eq!(to_pixel(v, 0, len), p, "len {len} pixel {p}");
                // Also correct if Windows rounded instead of truncating.
                let rounded = (i64::from(v) * i64::from(len) + 32768) >> 16;
                assert_eq!(rounded, p, "rounding inverse, len {len} pixel {p}");
            }
        }
    }

    #[test]
    fn single_monitor_corners() {
        let m = ScreenArea::new(0, 0, 1920, 1080);
        assert_eq!(landing(0.0, 0.0, m, m), (0, 0));
        assert_eq!(landing(1.0, 1.0, m, m), (1919, 1079));
        assert_eq!(landing(0.5, 0.5, m, m), (960, 540));
        assert_eq!(landing(0.999_999, 0.0, m, m), (1919, 0));
    }

    #[test]
    fn far_edge_never_exceeds_max() {
        let m = ScreenArea::new(0, 0, 1, 1);
        let p = to_absolute(1.0, 1.0, &m, &m).unwrap();
        assert!(p.x <= ABSOLUTE_MAX && p.y <= ABSOLUTE_MAX);
        assert_eq!(landing(1.0, 1.0, m, m), (0, 0));
    }

    #[test]
    fn out_of_range_is_clamped() {
        let m = ScreenArea::new(0, 0, 1920, 1080);
        assert_eq!(landing(-3.0, 2.0, m, m), (0, 1079));
        assert_eq!(landing(f32::MAX, f32::MIN, m, m), (1919, 0));
    }

    #[test]
    fn non_finite_is_rejected() {
        let m = ScreenArea::new(0, 0, 1920, 1080);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(to_absolute(bad, 0.5, &m, &m).is_none());
            assert!(to_absolute(0.5, bad, &m, &m).is_none());
        }
    }

    #[test]
    fn empty_rectangles_are_rejected() {
        let m = ScreenArea::new(0, 0, 1920, 1080);
        let empty = ScreenArea::new(0, 0, 0, 1080);
        assert!(to_absolute(0.5, 0.5, &empty, &m).is_none());
        assert!(to_absolute(0.5, 0.5, &m, &empty).is_none());
    }

    #[test]
    fn secondary_monitor_right_of_primary() {
        let primary = ScreenArea::new(0, 0, 1920, 1080);
        let right = ScreenArea::new(1920, 0, 2560, 1440);
        let desktop = ScreenArea::new(0, 0, 4480, 1440);
        assert_eq!(landing(0.0, 0.0, right, desktop), (1920, 0));
        assert_eq!(landing(1.0, 1.0, right, desktop), (4479, 1439));
        assert_eq!(landing(1.0, 1.0, primary, desktop), (1919, 1079));
    }

    #[test]
    fn monitor_with_negative_origin() {
        // Monitor left of and above the primary: desktop origin is negative.
        let left = ScreenArea::new(-2560, -360, 2560, 1440);
        let primary = ScreenArea::new(0, 0, 1920, 1080);
        let desktop = ScreenArea::new(-2560, -360, 4480, 1440);
        assert_eq!(landing(0.0, 0.0, left, desktop), (-2560, -360));
        assert_eq!(landing(1.0, 1.0, left, desktop), (-1, 1079));
        assert_eq!(landing(0.5, 0.5, left, desktop), (-1280, 360));
        assert_eq!(landing(0.0, 0.0, primary, desktop), (0, 0));
        assert_eq!(landing(1.0, 1.0, primary, desktop), (1919, 1079));
    }

    #[test]
    fn stale_monitor_outside_desktop_is_clamped() {
        // Monitor unplugged: its rectangle is no longer part of the desktop.
        let gone = ScreenArea::new(1920, 0, 1920, 1080);
        let desktop = ScreenArea::new(0, 0, 1920, 1080);
        assert_eq!(landing(0.5, 0.5, gone, desktop), (1919, 540));
    }
}
