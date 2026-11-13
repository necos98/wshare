//! Translation of local window input (winit) into remote-control events.
//! Pure functions: the window decides *when* to send, this decides *what*.

use protocol::input::{InputEvent, MouseButton as RemoteButton, WHEEL_DELTA};
use render::{Rect, fit_rect};
use winit::event::{ElementState, MouseButton, MouseScrollDelta};
use winit::keyboard::PhysicalKey;
use winit::platform::scancode::PhysicalKeyExtScancode;

use super::key_router::RawKey;

/// Touchpads report pixels; this many pixels count as one wheel notch.
const PIXELS_PER_NOTCH: f64 = 40.0;

/// Maps a pointer position in the window to the shared screen, normalized
/// to 0..1, accounting for the letterbox bars around the video. Positions in
/// the bars are clamped to the nearest screen edge, so edges (taskbar, hot
/// corners) stay reachable. `None` when the window has no video area.
pub fn normalize_pointer(
    position: (f64, f64),
    window: (u32, u32),
    stream: (u32, u32),
) -> Option<(f32, f32)> {
    let video: Rect = fit_rect(stream, window);
    if video.width() <= 0 || video.height() <= 0 {
        return None;
    }
    // Pixel centres: the first pixel maps to 0.5/w, the last to (w-0.5)/w,
    // so the host's floor(x * width) lands on the same pixel.
    let axis = |pos: f64, start: i32, len: i32| {
        let pixel = (pos - f64::from(start))
            .floor()
            .clamp(0.0, f64::from(len - 1));
        ((pixel + 0.5) / f64::from(len)) as f32
    };
    Some((
        axis(position.0, video.left, video.width()),
        axis(position.1, video.top, video.height()),
    ))
}

pub fn map_button(button: MouseButton) -> Option<RemoteButton> {
    match button {
        MouseButton::Left => Some(RemoteButton::Left),
        MouseButton::Right => Some(RemoteButton::Right),
        MouseButton::Middle => Some(RemoteButton::Middle),
        MouseButton::Back => Some(RemoteButton::Back),
        MouseButton::Forward => Some(RemoteButton::Forward),
        MouseButton::Other(_) => None,
    }
}

/// Wheel delta in [`WHEEL_DELTA`] units; `None` if it rounds to nothing.
/// winit and Win32 share the sign convention (positive = away / right).
pub fn map_wheel(delta: MouseScrollDelta) -> Option<InputEvent> {
    let (x, y) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (f64::from(x), f64::from(y)),
        MouseScrollDelta::PixelDelta(p) => (p.x / PIXELS_PER_NOTCH, p.y / PIXELS_PER_NOTCH),
    };
    let units = |notches: f64| (notches * f64::from(WHEEL_DELTA)).round() as i32;
    let (delta_x, delta_y) = (units(x), units(y));
    (delta_x != 0 || delta_y != 0).then_some(InputEvent::MouseWheel { delta_x, delta_y })
}

/// A window key event as a scan-code [`RawKey`] for the [`KeyRouter`].
/// winit encodes extended keys with an 0xE0 prefix byte (e.g. Right Ctrl =
/// 0xE01D); it is split out here.
///
/// [`KeyRouter`]: super::key_router::KeyRouter
pub fn raw_key(key: PhysicalKey, state: ElementState) -> Option<RawKey> {
    let code = key.to_scancode()?;
    let extended = code & 0xFF00 == 0xE000;
    if code & 0xFF == 0 || (code > 0xFF && !extended) {
        return None; // unknown, or a prefix we cannot express (e.g. Pause's 0xE1)
    }
    Some(RawKey {
        scan: code & 0xFF,
        extended,
        pressed: state.is_pressed(),
    })
}

#[cfg(test)]
mod tests {
    use winit::dpi::PhysicalPosition;
    use winit::keyboard::KeyCode;

    use super::*;

    const FULL_HD: (u32, u32) = (1920, 1080);

    #[test]
    fn pointer_maps_pixel_centres_without_bars() {
        let (x, y) = normalize_pointer((0.0, 0.0), FULL_HD, FULL_HD).unwrap();
        assert_eq!((x, y), (0.5 / 1920.0, 0.5 / 1080.0));
        let (x, y) = normalize_pointer((1919.9, 1079.2), FULL_HD, FULL_HD).unwrap();
        assert_eq!((x, y), (1919.5 / 1920.0, 1079.5 / 1080.0));
    }

    #[test]
    fn pointer_accounts_for_letterbox_and_clamps_in_bars() {
        // 1920x1080 stream in a 1000x1000 window: video is 1000x563 at top 218.
        let window = (1000, 1000);
        let (_, y) = normalize_pointer((500.0, 0.0), window, FULL_HD).unwrap();
        assert!(y < 0.001, "top bar clamps to the top edge, got {y}");
        let (_, y) = normalize_pointer((500.0, 999.0), window, FULL_HD).unwrap();
        assert!(y > 0.999, "bottom bar clamps to the bottom edge, got {y}");
        let (x, y) = normalize_pointer((500.0, 500.0), window, FULL_HD).unwrap();
        assert!(
            (x - 0.5).abs() < 0.002 && (y - 0.5).abs() < 0.002,
            "{x},{y}"
        );
    }

    #[test]
    fn pointer_needs_a_visible_video_area() {
        assert_eq!(normalize_pointer((1.0, 1.0), (0, 0), FULL_HD), None);
    }

    #[test]
    fn buttons() {
        assert_eq!(map_button(MouseButton::Left), Some(RemoteButton::Left));
        assert_eq!(
            map_button(MouseButton::Forward),
            Some(RemoteButton::Forward)
        );
        assert_eq!(map_button(MouseButton::Other(9)), None);
    }

    #[test]
    fn wheel_lines_and_pixels() {
        assert_eq!(
            map_wheel(MouseScrollDelta::LineDelta(0.0, -1.0)),
            Some(InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: -120
            })
        );
        assert_eq!(
            map_wheel(MouseScrollDelta::LineDelta(0.5, 0.0)),
            Some(InputEvent::MouseWheel {
                delta_x: 60,
                delta_y: 0
            })
        );
        assert_eq!(
            map_wheel(MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                0.0, 40.0
            ))),
            Some(InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: 120
            })
        );
        assert_eq!(map_wheel(MouseScrollDelta::LineDelta(0.0, 0.001)), None);
    }

    #[test]
    fn plain_and_extended_keys() {
        let a = raw_key(PhysicalKey::Code(KeyCode::KeyA), ElementState::Pressed);
        assert_eq!(
            a,
            Some(RawKey {
                scan: 0x1E,
                extended: false,
                pressed: true
            })
        );
        let right_ctrl = raw_key(
            PhysicalKey::Code(KeyCode::ControlRight),
            ElementState::Released,
        );
        assert_eq!(
            right_ctrl,
            Some(RawKey {
                scan: 0x1D,
                extended: true,
                pressed: false
            })
        );
        let up = raw_key(PhysicalKey::Code(KeyCode::ArrowUp), ElementState::Pressed);
        assert_eq!(
            up,
            Some(RawKey {
                scan: 0x48,
                extended: true,
                pressed: true
            })
        );
        let enter = raw_key(PhysicalKey::Code(KeyCode::Enter), ElementState::Pressed);
        assert_eq!(
            enter,
            Some(RawKey {
                scan: 0x1C,
                extended: false,
                pressed: true
            })
        );
    }
}
