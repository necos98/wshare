//! Geometry of the full-screen control bar. CONTRACT shared by the
//! renderer (drawing) and the app (hit-testing mouse clicks): both must use
//! these exact rectangles, so this is the single source of truth.
//!
//! Layout (all in physical pixels of the window's client area, centred
//! horizontally, touching the top edge):
//! - collapsed: only a small rounded "handle" tab is drawn;
//! - expanded: a bar holding `button_count` equal-width buttons side by
//!   side (the handle is not drawn then, but its rect is still reported).
//!
//! Sizes scale with `dpi_scale` (1.0 = 96 DPI). Rects are clamped to the
//! window; a window too small for the bar still yields valid (possibly
//! clipped) rects, never negative sizes.

use crate::layout::Rect;

/// What the pointer is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarHit {
    None,
    Handle,
    /// Inside the expanded bar but not on a button (padding/gaps).
    Bar,
    Button(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolbarGeometry {
    pub handle: Rect,
    /// The whole expanded bar (meaningful only when expanded; empty when
    /// there are no buttons).
    pub bar: Rect,
    /// One rect per button, inside `bar`, left to right.
    pub buttons: Vec<Rect>,
    pub expanded: bool,
}

// Sizes at 96 DPI (scale 1.0), in pixels.
const HANDLE_W: f32 = 56.0;
const HANDLE_H: f32 = 8.0;
const BAR_H: f32 = 40.0;
const PADDING: f32 = 8.0;
const BUTTON_W: f32 = 110.0;
const GAP: f32 = 6.0;

/// A 96-DPI size scaled and snapped to whole pixels.
fn px(size: f32, scale: f32) -> i32 {
    (size * scale).round() as i32
}

/// A `width` x `height` rect horizontally centred in the window, touching
/// the top edge, clamped to the window.
fn top_centered(window: (i32, i32), width: i32, height: i32) -> Rect {
    let width = width.clamp(0, window.0);
    let height = height.clamp(0, window.1);
    let left = (window.0 - width) / 2;
    Rect {
        left,
        top: 0,
        right: left + width,
        bottom: height,
    }
}

/// Computes the bar layout. Pure.
pub fn toolbar_geometry(
    window: (u32, u32),
    dpi_scale: f32,
    button_count: usize,
    expanded: bool,
) -> ToolbarGeometry {
    let scale = if dpi_scale.is_finite() && dpi_scale > 0.0 {
        dpi_scale.min(16.0)
    } else {
        1.0
    };
    // Keep every intermediate sum far from i32 overflow.
    let window = (window.0.min(1 << 20) as i32, window.1.min(1 << 20) as i32);
    let handle = top_centered(window, px(HANDLE_W, scale), px(HANDLE_H, scale));

    let (bar, buttons) = if expanded && button_count > 0 {
        let (pad, gap) = (px(PADDING, scale), px(GAP, scale));
        let count = i32::try_from(button_count.min(1 << 16)).unwrap_or(1 << 16);
        let gaps = gap * (count - 1);
        // Buttons shrink (down to nothing) when the window is too narrow.
        let room = (window.0 - 2 * pad - gaps).max(0);
        let button_w = px(BUTTON_W, scale).min(room / count);
        let bar_w = 2 * pad + gaps + button_w * count;
        let bar = top_centered(window, bar_w, px(BAR_H, scale));
        let top = (bar.top + pad).min(bar.bottom);
        let bottom = (bar.bottom - pad).max(top);
        let mut left = bar.left + pad;
        let buttons = (0..button_count.min(1 << 16))
            .map(|_| {
                let rect = Rect {
                    left,
                    top,
                    right: left + button_w,
                    bottom,
                };
                left += button_w + gap;
                rect
            })
            .collect();
        (bar, buttons)
    } else {
        (Rect::default(), Vec::new())
    };
    ToolbarGeometry {
        handle,
        bar,
        buttons,
        expanded,
    }
}

fn contains(rect: &Rect, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

impl ToolbarGeometry {
    /// What a pointer at (x, y) is over. When collapsed only the handle
    /// counts; when expanded the bar and its buttons count (and the handle
    /// is ignored).
    pub fn hit(&self, x: i32, y: i32) -> ToolbarHit {
        if !self.expanded {
            return if contains(&self.handle, x, y) {
                ToolbarHit::Handle
            } else {
                ToolbarHit::None
            };
        }
        if let Some(index) = self.buttons.iter().position(|b| contains(b, x, y)) {
            ToolbarHit::Button(index)
        } else if contains(&self.bar, x, y) {
            ToolbarHit::Bar
        } else {
            ToolbarHit::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: (u32, u32) = (1920, 1080);

    #[test]
    fn handle_is_centred_pill_at_top() {
        let g = toolbar_geometry(WIN, 1.0, 3, false);
        let expected = Rect {
            left: 932,
            top: 0,
            right: 988,
            bottom: 8,
        };
        assert_eq!(g.handle, expected);
        assert!(!g.expanded);
        assert!(g.buttons.is_empty());
    }

    #[test]
    fn expanded_bar_is_centred_and_sized_from_buttons() {
        let g = toolbar_geometry(WIN, 1.0, 3, true);
        // 3*110 + 2*6 + 2*8 = 358
        assert_eq!(g.bar.width(), 358);
        assert_eq!(g.bar.height(), 40);
        assert_eq!(g.bar.top, 0);
        assert_eq!(g.bar.left + g.bar.right, 1920, "centred");
        assert_eq!(g.buttons.len(), 3);
        assert_eq!(g.buttons[0].left, g.bar.left + 8);
        assert_eq!(g.buttons[0].width(), 110);
        assert_eq!(g.buttons[0].height(), 24);
        assert_eq!(g.buttons[1].left - g.buttons[0].right, 6);
        assert_eq!(g.buttons[2].right, g.bar.right - 8);
    }

    #[test]
    fn scales_with_dpi() {
        for (scale, handle_w, bar_h, button_w) in
            [(1.0, 56, 40, 110), (1.5, 84, 60, 165), (2.0, 112, 80, 220)]
        {
            let g = toolbar_geometry((3840, 2160), scale, 2, true);
            assert_eq!(g.handle.width(), handle_w, "scale {scale}");
            assert_eq!(g.bar.height(), bar_h, "scale {scale}");
            assert_eq!(g.buttons[0].width(), button_w, "scale {scale}");
        }
    }

    #[test]
    fn buttons_are_inside_bar_and_disjoint() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for count in 1..6 {
                let g = toolbar_geometry((1280, 720), scale, count, true);
                assert_eq!(g.buttons.len(), count);
                for (i, b) in g.buttons.iter().enumerate() {
                    assert!(b.left >= g.bar.left && b.right <= g.bar.right);
                    assert!(b.top >= g.bar.top && b.bottom <= g.bar.bottom);
                    if let Some(next) = g.buttons.get(i + 1) {
                        assert!(b.right < next.left, "gap between buttons");
                    }
                }
            }
        }
    }

    #[test]
    fn hit_expanded() {
        let g = toolbar_geometry(WIN, 1.0, 3, true);
        for (i, b) in g.buttons.iter().enumerate() {
            assert_eq!(g.hit(b.left, b.top), ToolbarHit::Button(i));
            assert_eq!(g.hit(b.right - 1, b.bottom - 1), ToolbarHit::Button(i));
            assert_eq!(g.hit(b.right, b.top), ToolbarHit::Bar);
        }
        let gap = g.buttons[0].right + 2;
        assert_eq!(g.hit(gap, 20), ToolbarHit::Bar);
        assert_eq!(g.hit(g.bar.left + 1, 1), ToolbarHit::Bar, "padding");
        assert_eq!(g.hit(g.bar.left - 1, 10), ToolbarHit::None);
        assert_eq!(g.hit(g.bar.right, 10), ToolbarHit::None);
        assert_eq!(g.hit(960, g.bar.bottom), ToolbarHit::None);
        assert_eq!(g.hit(960, -1), ToolbarHit::None);
    }

    #[test]
    fn hit_collapsed_only_handle() {
        let g = toolbar_geometry(WIN, 1.0, 3, false);
        assert_eq!(g.hit(960, 0), ToolbarHit::Handle);
        assert_eq!(g.hit(g.handle.left, 7), ToolbarHit::Handle);
        assert_eq!(g.hit(g.handle.right, 4), ToolbarHit::None);
        assert_eq!(g.hit(960, 8), ToolbarHit::None);
        assert_eq!(g.hit(0, 0), ToolbarHit::None);
    }

    #[test]
    fn bar_is_clamped_to_narrow_window() {
        let g = toolbar_geometry((200, 100), 1.0, 3, true);
        assert!(g.bar.left >= 0 && g.bar.right <= 200);
        for b in &g.buttons {
            assert!(b.left >= g.bar.left && b.right <= g.bar.right && b.width() >= 0);
        }
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        let windows = [
            (0, 0),
            (1, 1),
            (5, 0),
            (0, 50),
            (10, 10),
            (u32::MAX, u32::MAX),
        ];
        for window in windows {
            for count in [0, 1, 3, 1000] {
                for expanded in [false, true] {
                    for scale in [0.0, -1.0, f32::NAN, f32::INFINITY, 1.0, 100.0] {
                        let g = toolbar_geometry(window, scale, count, expanded);
                        for r in [&g.handle, &g.bar].into_iter().chain(&g.buttons) {
                            assert!(r.width() >= 0 && r.height() >= 0, "{r:?}");
                            assert!(r.left >= 0 && r.top >= 0);
                        }
                        g.hit(0, 0);
                    }
                }
            }
        }
    }

    #[test]
    fn zero_buttons_has_no_bar() {
        let g = toolbar_geometry(WIN, 1.0, 0, true);
        assert!(g.buttons.is_empty());
        assert_eq!(g.hit(960, 10), ToolbarHit::None);
    }
}
