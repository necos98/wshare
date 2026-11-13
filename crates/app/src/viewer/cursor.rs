//! Shows the host's pointer shape as the viewer window's own cursor, so the
//! pointer follows the local mouse with zero network latency.

use std::collections::HashMap;

use protocol::input::CursorShape;
use winit::event_loop::ActiveEventLoop;
use winit::window::{CursorIcon, CustomCursor, Window};

/// Converted cursors are kept by shape id: the host alternates between a
/// few shapes (arrow, I-beam, hand...), each converted only once.
#[derive(Default)]
pub struct CursorCache {
    cursors: HashMap<u64, CustomCursor>,
    /// Id of the host shape last applied (reapplied after the arrow).
    current: Option<u64>,
    /// The host hid its pointer.
    host_hidden: bool,
    /// The local control bar is under the pointer: arrow, always visible.
    arrow: bool,
}

/// Bounds the cache in case a host animates its cursor (each frame of an
/// animated cursor is a distinct shape).
const MAX_CACHED: usize = 64;

impl CursorCache {
    /// Applies `shape` to `window`. Invalid shapes (untrusted input) are
    /// ignored and the current cursor stays.
    pub fn apply(&mut self, event_loop: &ActiveEventLoop, window: &Window, shape: CursorShape) {
        if !shape.is_valid() {
            return;
        }
        if !self.cursors.contains_key(&shape.id) {
            let Ok(source) = CustomCursor::from_rgba(
                shape.rgba,
                shape.width,
                shape.height,
                shape.hotspot_x,
                shape.hotspot_y,
            ) else {
                return;
            };
            if self.cursors.len() >= MAX_CACHED {
                self.cursors.clear();
            }
            self.cursors
                .insert(shape.id, event_loop.create_custom_cursor(source));
        }
        self.current = Some(shape.id);
        self.show(window);
    }

    /// The host shows or hides its pointer.
    pub fn set_host_visible(&mut self, window: &Window, visible: bool) {
        self.host_hidden = !visible;
        self.show(window);
    }

    /// The pointer is on (`true`) or left the local control bar: show the
    /// normal arrow, then restore the host's shape.
    pub fn set_arrow(&mut self, window: &Window, arrow: bool) {
        if self.arrow != arrow {
            self.arrow = arrow;
            self.show(window);
        }
    }

    fn show(&self, window: &Window) {
        if self.arrow {
            window.set_cursor(CursorIcon::Default);
            window.set_cursor_visible(true);
            return;
        }
        match self.current.and_then(|id| self.cursors.get(&id)) {
            Some(cursor) => window.set_cursor(cursor.clone()),
            None => window.set_cursor(CursorIcon::Default),
        }
        window.set_cursor_visible(!self.host_hidden);
    }
}
