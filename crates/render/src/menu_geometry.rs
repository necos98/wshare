//! Geometry of the drop-down list a control-bar button opens (e.g. the
//! viewer's "Screen" and "Quality" menus). CONTRACT shared, like
//! [`crate::toolbar_geometry`], by the renderer (drawing) and the app
//! (hit-testing): both use these exact rectangles.
//!
//! The list hangs under its button, a little below the bar, one row per
//! item, kept inside the window. Rows that do not fit in a window too short
//! for them are left out; every rect is valid (never a negative size).

use crate::layout::Rect;
use crate::toolbar_geometry::{ToolbarGeometry, px, sane_scale, sane_window};

/// What the pointer is over, among the menu's parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuHit {
    None,
    /// Inside the list but between rows (its padding).
    Panel,
    Item(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuGeometry {
    /// The whole list.
    pub panel: Rect,
    /// One rect per row shown, top to bottom, inside `panel`.
    pub items: Vec<Rect>,
}

// Sizes at 96 DPI (scale 1.0), in pixels.
const MENU_W: f32 = 240.0;
const ITEM_H: f32 = 28.0;
const PADDING: f32 = 4.0;
/// Space between the bar and the list.
const GAP: f32 = 4.0;

/// Where the list of `item_count` rows of `button` goes, given the bar's
/// layout. `None` when the bar has no such button (collapsed) or there is
/// nothing to list. Pure.
pub fn menu_geometry(
    bar: &ToolbarGeometry,
    button: usize,
    item_count: usize,
    window: (u32, u32),
    dpi_scale: f32,
) -> Option<MenuGeometry> {
    let anchor = bar.buttons.get(button)?;
    if item_count == 0 {
        return None;
    }
    let scale = sane_scale(dpi_scale);
    let window = sane_window(window);
    let (pad, row) = (px(PADDING, scale), px(ITEM_H, scale).max(1));
    let width = px(MENU_W, scale).max(anchor.width()).min(window.0).max(0);
    let left = anchor.left.clamp(0, (window.0 - width).max(0));
    let top = (bar.bar.bottom + px(GAP, scale)).clamp(0, window.1);
    let room = (window.1 - top - 2 * pad).max(0) / row;
    let rows = usize::try_from(room).unwrap_or(0).min(item_count);
    let items = (0..rows)
        .map(|i| {
            let row_top = top + pad + row * i as i32;
            Rect {
                left: left + pad.min(width),
                top: row_top,
                right: (left + width - pad).max(left + pad.min(width)),
                bottom: row_top + row,
            }
        })
        .collect::<Vec<_>>();
    let bottom = items
        .last()
        .map_or(top, |last| last.bottom + pad)
        .min(window.1);
    Some(MenuGeometry {
        panel: Rect {
            left,
            top,
            right: left + width,
            bottom: bottom.max(top),
        },
        items,
    })
}

fn contains(rect: &Rect, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

impl MenuGeometry {
    /// What a pointer at (x, y) is over.
    pub fn hit(&self, x: i32, y: i32) -> MenuHit {
        if let Some(index) = self.items.iter().position(|r| contains(r, x, y)) {
            MenuHit::Item(index)
        } else if contains(&self.panel, x, y) {
            MenuHit::Panel
        } else {
            MenuHit::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolbar_geometry::toolbar_geometry;

    const WIN: (u32, u32) = (1920, 1080);

    fn bar() -> ToolbarGeometry {
        toolbar_geometry(WIN, 1.0, 5, true)
    }

    #[test]
    fn the_list_hangs_under_its_button() {
        let g = bar();
        let menu = menu_geometry(&g, 1, 3, WIN, 1.0).unwrap();
        assert_eq!(menu.panel.left, g.buttons[1].left);
        assert_eq!(menu.panel.top, g.bar.bottom + 4);
        assert_eq!(menu.panel.width(), 240);
        assert_eq!(menu.items.len(), 3);
        assert_eq!(menu.items[0].top, menu.panel.top + 4);
        assert_eq!(menu.items[0].height(), 28);
        assert_eq!(menu.items[1].top, menu.items[0].bottom, "rows touch");
        assert_eq!(menu.panel.bottom, menu.items[2].bottom + 4);
        for item in &menu.items {
            assert!(item.left >= menu.panel.left && item.right <= menu.panel.right);
        }
    }

    #[test]
    fn hits_tell_rows_padding_and_outside_apart() {
        let menu = menu_geometry(&bar(), 0, 2, WIN, 1.0).unwrap();
        let first = menu.items[0];
        assert_eq!(menu.hit(first.left, first.top), MenuHit::Item(0));
        assert_eq!(menu.hit(first.left, first.bottom), MenuHit::Item(1));
        assert_eq!(menu.hit(menu.panel.left, menu.panel.top), MenuHit::Panel);
        assert_eq!(menu.hit(menu.panel.right, first.top), MenuHit::None);
        assert_eq!(menu.hit(first.left, menu.panel.bottom), MenuHit::None);
    }

    #[test]
    fn the_list_stays_inside_a_narrow_window() {
        let window = (300, 400);
        let g = toolbar_geometry(window, 1.0, 5, true);
        for button in 0..5 {
            let menu = menu_geometry(&g, button, 4, window, 1.0).unwrap();
            assert!(menu.panel.left >= 0 && menu.panel.right <= 300, "{menu:?}");
        }
    }

    #[test]
    fn rows_that_do_not_fit_a_short_window_are_left_out() {
        let window = (1920, 120);
        let g = toolbar_geometry(window, 1.0, 5, true);
        let menu = menu_geometry(&g, 0, 10, window, 1.0).unwrap();
        assert!(menu.items.len() < 10);
        assert!(menu.panel.bottom <= 120);
        assert!(menu.items.iter().all(|r| r.bottom <= 120));
    }

    #[test]
    fn sizes_scale_with_dpi() {
        let g = toolbar_geometry((3840, 2160), 2.0, 5, true);
        let menu = menu_geometry(&g, 2, 2, (3840, 2160), 2.0).unwrap();
        assert_eq!(menu.panel.width(), 480);
        assert_eq!(menu.items[0].height(), 56);
    }

    #[test]
    fn no_list_without_a_button_or_items() {
        let collapsed = toolbar_geometry(WIN, 1.0, 5, false);
        assert_eq!(menu_geometry(&collapsed, 0, 3, WIN, 1.0), None);
        assert_eq!(menu_geometry(&bar(), 5, 3, WIN, 1.0), None);
        assert_eq!(menu_geometry(&bar(), 0, 0, WIN, 1.0), None);
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        for window in [(0, 0), (1, 1), (10, 10), (u32::MAX, u32::MAX)] {
            for scale in [0.0, f32::NAN, 1.0, 100.0] {
                let g = toolbar_geometry(window, scale, 5, true);
                for count in [1, 3, 1000] {
                    if let Some(menu) = menu_geometry(&g, 0, count, window, scale) {
                        for r in std::iter::once(&menu.panel).chain(&menu.items) {
                            assert!(r.width() >= 0 && r.height() >= 0, "{r:?}");
                        }
                        menu.hit(0, 0);
                    }
                }
            }
        }
    }
}
