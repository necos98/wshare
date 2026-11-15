//! Remote control payloads: input events (viewer -> host) and the mouse
//! cursor's appearance (host -> viewer). Carried by control messages.

use serde::{Deserialize, Serialize};

/// Wheel units per notch, as in Win32 `WHEEL_DELTA`.
pub const WHEEL_DELTA: i32 = 120;

/// Largest cursor image accepted (Windows accessibility cursors reach 256).
pub const MAX_CURSOR_SIDE: u16 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// "Back" side button (XBUTTON1).
    Back,
    /// "Forward" side button (XBUTTON2).
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    /// Absolute pointer position, normalized to the shared screen:
    /// (0,0) = top-left corner, (1,1) = bottom-right. Resolution-independent,
    /// so the viewer window can have any size.
    MouseMove {
        x: f32,
        y: f32,
    },
    MouseButton {
        button: MouseButton,
        pressed: bool,
    },
    /// Wheel rotation in [`WHEEL_DELTA`] units (positive = away from the
    /// user / to the right). Fractions of a notch come from touchpads.
    MouseWheel {
        delta_x: i32,
        delta_y: i32,
    },
    /// A key by its PS/2 set-1 scan code (position on the keyboard, not the
    /// character): the HOST's keyboard layout decides what it types.
    /// `extended` = the 0xE0-prefixed keys (arrows, right Ctrl/Alt, ...).
    Key {
        scancode: u16,
        extended: bool,
        pressed: bool,
    },
    /// Release every key and button the viewer pressed (it lost focus, so
    /// it will not see their release).
    ReleaseAll,
}

/// The pointer's look. Sent when it changes; the viewer shows it as its own
/// cursor, so pointer motion has no network latency at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorShape {
    /// Content hash, so the viewer can cache converted cursors.
    pub id: u64,
    pub width: u16,
    pub height: u16,
    pub hotspot_x: u16,
    pub hotspot_y: u16,
    /// Straight (non-premultiplied) RGBA, row-major, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl CursorShape {
    /// Checks the invariants a decoded (untrusted) shape must satisfy.
    pub fn is_valid(&self) -> bool {
        let side_ok = |s: u16| (1..=MAX_CURSOR_SIDE).contains(&s);
        side_ok(self.width)
            && side_ok(self.height)
            && self.hotspot_x < self.width
            && self.hotspot_y < self.height
            && self.rgba.len() == usize::from(self.width) * usize::from(self.height) * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(width: u16, height: u16) -> CursorShape {
        CursorShape {
            id: 1,
            width,
            height,
            hotspot_x: 0,
            hotspot_y: 0,
            rgba: vec![0; usize::from(width) * usize::from(height) * 4],
        }
    }

    #[test]
    fn valid_shapes() {
        assert!(shape(32, 32).is_valid());
        assert!(shape(1, 1).is_valid());
        assert!(shape(MAX_CURSOR_SIDE, MAX_CURSOR_SIDE).is_valid());
    }

    #[test]
    fn invalid_shapes() {
        assert!(!shape(0, 32).is_valid());
        assert!(!shape(MAX_CURSOR_SIDE + 1, 1).is_valid());
        let mut short = shape(4, 4);
        short.rgba.pop();
        assert!(!short.is_valid());
        let mut hotspot = shape(4, 4);
        hotspot.hotspot_x = 4;
        assert!(!hotspot.is_valid());
    }
}
