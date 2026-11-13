//! The events handed to `SendInput`, built from protocol events.
//!
//! [`SyntheticInput`] is a safe, comparable mirror of the Win32 `INPUT`
//! union holding exactly the fields we set. Policy code and tests work with
//! it; only [`SyntheticInput::to_win32`] touches the union (a plain field
//! copy), so no `unsafe` union reads are needed anywhere.

use protocol::input::MouseButton;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{XBUTTON1, XBUTTON2};

use crate::geometry::AbsolutePoint;
use crate::keys::Scancode;

/// Tag stored in `dwExtraInfo` of every injected event ("WSHR"), so a
/// low-level hook in the app can tell remote input from local input.
pub const INJECTED_TAG: usize = 0x5753_4852;

/// One event for `SendInput`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntheticInput {
    Mouse {
        dx: i32,
        dy: i32,
        data: u32,
        flags: MOUSE_EVENT_FLAGS,
    },
    Keyboard {
        scan: u16,
        flags: KEYBD_EVENT_FLAGS,
    },
}

impl SyntheticInput {
    /// Absolute move on the virtual desktop.
    pub fn move_to(p: AbsolutePoint) -> Self {
        Self::Mouse {
            dx: p.x,
            dy: p.y,
            data: 0,
            flags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        }
    }

    /// Button press/release at the current pointer position (no move flag,
    /// so dx/dy are ignored).
    pub fn button(button: MouseButton, pressed: bool) -> Self {
        let (down, up, data) = match button {
            MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
            MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
            MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
            MouseButton::Back => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, u32::from(XBUTTON1)),
            MouseButton::Forward => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, u32::from(XBUTTON2)),
        };
        Self::Mouse {
            dx: 0,
            dy: 0,
            data,
            flags: if pressed { down } else { up },
        }
    }

    /// Wheel rotation: vertical then horizontal, zero axes omitted.
    /// The protocol's signs match Win32 (positive = away from the user for
    /// `WHEEL`, to the right for `HWHEEL`); `mouseData` is a `DWORD` holding
    /// a signed value, hence the two's-complement cast.
    pub fn wheel(delta_x: i32, delta_y: i32) -> Vec<Self> {
        [(delta_y, MOUSEEVENTF_WHEEL), (delta_x, MOUSEEVENTF_HWHEEL)]
            .into_iter()
            .filter(|&(delta, _)| delta != 0)
            .map(|(delta, flags)| Self::Mouse {
                dx: 0,
                dy: 0,
                data: delta as u32,
                flags,
            })
            .collect()
    }

    /// Key press/release by scan code: the host's layout decides the
    /// character, matching what a physical keyboard would do.
    pub fn key(key: Scancode, pressed: bool) -> Self {
        let mut flags = KEYEVENTF_SCANCODE;
        if key.extended() {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !pressed {
            flags |= KEYEVENTF_KEYUP;
        }
        Self::Keyboard {
            scan: u16::from(key.code()),
            flags,
        }
    }

    /// The Win32 `INPUT` for `SendInput`.
    pub fn to_win32(self) -> INPUT {
        match self {
            Self::Mouse {
                dx,
                dy,
                data,
                flags,
            } => INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx,
                        dy,
                        mouseData: data,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: INJECTED_TAG,
                    },
                },
            },
            Self::Keyboard { scan, flags } => INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: scan,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: INJECTED_TAG,
                    },
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse(data: u32, flags: MOUSE_EVENT_FLAGS) -> SyntheticInput {
        SyntheticInput::Mouse {
            dx: 0,
            dy: 0,
            data,
            flags,
        }
    }

    #[test]
    fn move_is_absolute_on_virtual_desktop() {
        let m = SyntheticInput::move_to(AbsolutePoint { x: 12, y: 65535 });
        assert_eq!(
            m,
            SyntheticInput::Mouse {
                dx: 12,
                dy: 65535,
                data: 0,
                flags: MOUSE_EVENT_FLAGS(0x0001 | 0x8000 | 0x4000)
            }
        );
    }

    #[test]
    fn every_button_maps() {
        use MouseButton::*;
        let cases = [
            (Left, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
            (Right, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
            (Middle, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
            (Back, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 1),
            (Forward, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 2),
        ];
        for (button, down, up, data) in cases {
            assert_eq!(
                SyntheticInput::button(button, true),
                mouse(data, down),
                "{button:?} down"
            );
            assert_eq!(
                SyntheticInput::button(button, false),
                mouse(data, up),
                "{button:?} up"
            );
        }
    }

    #[test]
    fn wheel_signs_and_axes() {
        assert_eq!(
            SyntheticInput::wheel(0, 120),
            vec![mouse(120, MOUSEEVENTF_WHEEL)]
        );
        assert_eq!(
            SyntheticInput::wheel(0, -120),
            vec![mouse((-120i32) as u32, MOUSEEVENTF_WHEEL)]
        );
        assert_eq!(
            SyntheticInput::wheel(30, 0),
            vec![mouse(30, MOUSEEVENTF_HWHEEL)]
        );
        assert_eq!(
            SyntheticInput::wheel(-240, 0),
            vec![mouse(0xFFFF_FF10, MOUSEEVENTF_HWHEEL)]
        );
        assert_eq!(
            SyntheticInput::wheel(-1, 2),
            vec![
                mouse(2, MOUSEEVENTF_WHEEL),
                mouse(u32::MAX, MOUSEEVENTF_HWHEEL)
            ]
        );
        assert!(SyntheticInput::wheel(0, 0).is_empty());
    }

    #[test]
    fn key_flags() {
        let a = Scancode::new(0x1E, false).unwrap();
        let right_alt = Scancode::new(0x38, true).unwrap();
        let kb = |scan, flags| SyntheticInput::Keyboard { scan, flags };
        assert_eq!(SyntheticInput::key(a, true), kb(0x1E, KEYEVENTF_SCANCODE));
        assert_eq!(
            SyntheticInput::key(a, false),
            kb(0x1E, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP)
        );
        assert_eq!(
            SyntheticInput::key(right_alt, true),
            kb(0x38, KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY)
        );
        assert_eq!(
            SyntheticInput::key(right_alt, false),
            kb(
                0x38,
                KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP
            )
        );
    }

    #[test]
    fn win32_conversion_copies_fields() {
        let m = SyntheticInput::button(MouseButton::Forward, true).to_win32();
        assert_eq!(m.r#type, INPUT_MOUSE);
        // SAFETY: `to_win32` wrote the `mi` variant for INPUT_MOUSE.
        let mi = unsafe { m.Anonymous.mi };
        assert_eq!(
            (mi.mouseData, mi.dwFlags, mi.dwExtraInfo, mi.time),
            (2, MOUSEEVENTF_XDOWN, INJECTED_TAG, 0)
        );

        let k = SyntheticInput::key(Scancode::new(0x48, true).unwrap(), false).to_win32();
        assert_eq!(k.r#type, INPUT_KEYBOARD);
        // SAFETY: `to_win32` wrote the `ki` variant for INPUT_KEYBOARD.
        let ki = unsafe { k.Anonymous.ki };
        assert_eq!(
            (ki.wVk, ki.wScan, ki.dwExtraInfo),
            (VIRTUAL_KEY(0), 0x48, INJECTED_TAG)
        );
        assert_eq!(
            ki.dwFlags,
            KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP
        );
    }
}
