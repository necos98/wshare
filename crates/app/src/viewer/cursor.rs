//! Shows the host's pointer shape as the viewer window's own cursor, so the
//! pointer follows the local mouse with zero network latency.

use std::collections::HashMap;

use protocol::input::CursorShape;
use winit::event_loop::ActiveEventLoop;
use winit::window::{CustomCursor, Window};

/// Converted cursors are kept by shape id: the host alternates between a
/// few shapes (arrow, I-beam, hand...), each converted only once.
#[derive(Default)]
pub struct CursorCache {
    cursors: HashMap<u64, CustomCursor>,
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
            self.cursors.insert(shape.id, event_loop.create_custom_cursor(source));
        }
        window.set_cursor(self.cursors[&shape.id].clone());
    }
}
