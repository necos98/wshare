//! Letterbox geometry.

/// A pixel rectangle (right/bottom exclusive), same layout as Win32 `RECT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

/// The largest rectangle with the aspect ratio of `source` that fits in
/// `target`, centred. The rest of `target` becomes black bars. Returns an
/// empty rectangle when either size is zero (e.g. a minimised window).
pub fn fit_rect(source: (u32, u32), target: (u32, u32)) -> Rect {
    let (sw, sh) = (source.0 as u64, source.1 as u64);
    let (tw, th) = (target.0 as u64, target.1 as u64);
    if sw == 0 || sh == 0 || tw == 0 || th == 0 {
        return Rect::default();
    }
    // Compare aspect ratios exactly with cross-multiplication (no floats).
    let (w, h) = if sw * th >= tw * sh {
        // Source is wider: full width, bars top and bottom.
        (tw, (sh * tw + sw / 2) / sw)
    } else {
        // Source is taller: full height, bars left and right.
        ((sw * th + sh / 2) / sh, th)
    };
    let (w, h) = (w.clamp(1, tw), h.clamp(1, th));
    let left = ((tw - w) / 2) as i32;
    let top = ((th - h) / 2) as i32;
    Rect {
        left,
        top,
        right: left + w as i32,
        bottom: top + h as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn same_aspect_fills_target() {
        assert_eq!(fit_rect((1920, 1080), (1280, 720)), rect(0, 0, 1280, 720));
    }

    #[test]
    fn wider_source_gets_bars_top_and_bottom() {
        // 16:9 in a square: 800x450 centred vertically.
        assert_eq!(fit_rect((1920, 1080), (800, 800)), rect(0, 175, 800, 625));
    }

    #[test]
    fn taller_source_gets_bars_left_and_right() {
        // 4:3 in 16:9: 1440x1080 centred horizontally.
        assert_eq!(
            fit_rect((1024, 768), (1920, 1080)),
            rect(240, 0, 1680, 1080)
        );
    }

    #[test]
    fn upscaling_keeps_aspect() {
        assert_eq!(fit_rect((640, 360), (2560, 1600)), rect(0, 80, 2560, 1520));
    }

    #[test]
    fn rounding_stays_inside_target() {
        let r = fit_rect((1920, 1080), (1001, 333));
        assert!(r.left >= 0 && r.top >= 0 && r.right <= 1001 && r.bottom <= 333);
        assert_eq!(r.height(), 333);
        assert_eq!(r.width(), 592);
    }

    #[test]
    fn zero_sizes_give_empty_rect() {
        assert_eq!(fit_rect((0, 1080), (800, 600)), Rect::default());
        assert_eq!(fit_rect((1920, 1080), (0, 0)), Rect::default());
    }

    #[test]
    fn extreme_aspect_keeps_at_least_one_pixel() {
        let r = fit_rect((100_000, 1), (100, 100));
        assert_eq!(r.height(), 1);
        assert_eq!(r.width(), 100);
    }
}
