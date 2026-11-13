//! Decides what happens to each key the viewer sees: forward it to the
//! host, treat it as a local hotkey, or leave it to Windows. Pure: both key
//! sources (the window's own key events, and the low-level hook used in full
//! screen) feed it scan codes and act on the answer.

use std::collections::HashSet;

use protocol::input::InputEvent;

const SCAN_ENTER: u16 = 0x1C;
const SCAN_CTRL: u16 = 0x1D;
const SCAN_I: u16 = 0x17;
const SCAN_ALT: u16 = 0x38;

/// AltGr makes the keyboard driver emit a fake Left Ctrl whose hook scan
/// code carries this bit (0x21D). The host's own layout regenerates it from
/// the forwarded Right Alt, so it is never forwarded nor counted as Ctrl.
pub const FAKE_ALTGR_CTRL_BIT: u32 = 0x200;

/// Viewer-side shortcuts, never sent to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    /// Ctrl+Alt+Enter.
    ToggleFullscreen,
    /// Ctrl+Alt+I.
    ToggleStats,
}

/// One key transition: PS/2 set-1 scan code (low byte, plus flag bits such
/// as [`FAKE_ALTGR_CTRL_BIT`]) and the 0xE0 "extended" prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawKey {
    pub scan: u32,
    pub extended: bool,
    pub pressed: bool,
}

impl RawKey {
    /// Identity of the physical key, regardless of press/release.
    fn id(self) -> (u16, bool) {
        ((self.scan & 0xFF) as u16, self.extended)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyAction {
    /// Run the hotkey locally and swallow the key. Ctrl/Alt were already
    /// forwarded, so the host must be told to release them.
    Hotkey(Hotkey),
    /// Send to the host (and swallow locally when a hook is in use).
    Forward(InputEvent),
    /// Swallow without forwarding.
    Swallow,
    /// Let Windows handle it as if we were not there.
    PassThrough,
}

#[derive(Default)]
pub struct KeyRouter {
    ctrl_down: HashSet<bool>,
    alt_down: HashSet<bool>,
    /// Keys whose press triggered a hotkey: repeats and release swallowed.
    hotkey_keys: HashSet<(u16, bool)>,
    /// Keys whose press this router has seen. A release without its press
    /// belongs to whoever saw the press — typically Windows, for keys held
    /// when the hook was installed (the Ctrl and Alt of the Ctrl+Alt+Enter
    /// that entered full screen). It must go back there, or Windows keeps
    /// the key down, even after the viewer closes.
    pressed: HashSet<(u16, bool)>,
}

impl KeyRouter {
    /// `forwarding`: remote control is on for this session.
    pub fn route(&mut self, key: RawKey, forwarding: bool) -> KeyAction {
        if key.scan & FAKE_ALTGR_CTRL_BIT != 0 {
            return if forwarding {
                KeyAction::Swallow
            } else {
                KeyAction::PassThrough
            };
        }
        self.track_modifiers(key);
        if key.pressed {
            self.pressed.insert(key.id());
        } else if !self.pressed.remove(&key.id()) {
            return KeyAction::PassThrough;
        }

        if key.pressed && self.hotkey_keys.contains(&key.id()) {
            return KeyAction::Swallow; // auto-repeat of a hotkey: run it once
        }
        if key.pressed
            && let Some(hotkey) = self.hotkey(key)
        {
            self.hotkey_keys.insert(key.id());
            return KeyAction::Hotkey(hotkey);
        }
        if !key.pressed && self.hotkey_keys.remove(&key.id()) {
            return KeyAction::Swallow;
        }
        if !forwarding {
            return KeyAction::PassThrough;
        }
        let (scancode, extended) = key.id();
        if scancode == 0 {
            return KeyAction::PassThrough; // no scan code: cannot express it
        }
        KeyAction::Forward(InputEvent::Key {
            scancode,
            extended,
            pressed: key.pressed,
        })
    }

    fn track_modifiers(&mut self, key: RawKey) {
        let (scancode, extended) = key.id();
        let set = match scancode {
            SCAN_CTRL => &mut self.ctrl_down,
            SCAN_ALT => &mut self.alt_down,
            _ => return,
        };
        if key.pressed {
            set.insert(extended);
        } else {
            set.remove(&extended);
        }
    }

    fn hotkey(&self, key: RawKey) -> Option<Hotkey> {
        if self.ctrl_down.is_empty() || self.alt_down.is_empty() {
            return None;
        }
        match key.id().0 {
            SCAN_ENTER => Some(Hotkey::ToggleFullscreen), // main or keypad Enter
            SCAN_I => Some(Hotkey::ToggleStats),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(scan: u32, pressed: bool) -> RawKey {
        RawKey {
            scan,
            extended: false,
            pressed,
        }
    }

    fn ext(scan: u32, pressed: bool) -> RawKey {
        RawKey {
            scan,
            extended: true,
            pressed,
        }
    }

    fn forwarded(scancode: u16, pressed: bool) -> KeyAction {
        KeyAction::Forward(InputEvent::Key {
            scancode,
            extended: false,
            pressed,
        })
    }

    #[test]
    fn plain_keys_are_forwarded_when_controlling() {
        let mut r = KeyRouter::default();
        assert_eq!(r.route(key(0x1E, true), true), forwarded(0x1E, true));
        assert_eq!(r.route(key(0x1E, false), true), forwarded(0x1E, false));
    }

    #[test]
    fn system_keys_are_forwarded_too() {
        let mut r = KeyRouter::default();
        assert_eq!(
            r.route(ext(0x5B, true), true),
            KeyAction::Forward(InputEvent::Key {
                scancode: 0x5B,
                extended: true,
                pressed: true
            })
        );
        // Alt+Tab: both forwarded, nothing local.
        assert_eq!(
            r.route(key(SCAN_ALT as u32, true), true),
            forwarded(0x38, true)
        );
        assert_eq!(r.route(key(0x0F, true), true), forwarded(0x0F, true));
    }

    #[test]
    fn view_only_passes_keys_through() {
        let mut r = KeyRouter::default();
        assert_eq!(r.route(key(0x1E, true), false), KeyAction::PassThrough);
    }

    #[test]
    fn ctrl_alt_enter_toggles_fullscreen_once() {
        let mut r = KeyRouter::default();
        r.route(key(0x1D, true), true);
        r.route(key(0x38, true), true);
        assert_eq!(
            r.route(key(0x1C, true), true),
            KeyAction::Hotkey(Hotkey::ToggleFullscreen)
        );
        // Holding Enter auto-repeats: the hotkey must not fire again.
        assert_eq!(r.route(key(0x1C, true), true), KeyAction::Swallow);
        assert_eq!(r.route(key(0x1C, true), true), KeyAction::Swallow);
        // Its release is swallowed too...
        assert_eq!(r.route(key(0x1C, false), true), KeyAction::Swallow);
        // ...and Enter alone afterwards is a normal key again.
        r.route(key(0x38, false), true);
        r.route(key(0x1D, false), true);
        assert_eq!(r.route(key(0x1C, true), true), forwarded(0x1C, true));
    }

    #[test]
    fn right_ctrl_right_alt_and_keypad_enter_work_too() {
        let mut r = KeyRouter::default();
        r.route(ext(0x1D, true), true);
        r.route(ext(0x38, true), true);
        assert_eq!(
            r.route(ext(0x1C, true), true),
            KeyAction::Hotkey(Hotkey::ToggleFullscreen)
        );
    }

    #[test]
    fn modifier_released_on_one_side_still_held_on_the_other() {
        let mut r = KeyRouter::default();
        r.route(key(0x1D, true), true);
        r.route(ext(0x1D, true), true);
        r.route(key(0x1D, false), true);
        r.route(key(0x38, true), true);
        assert_eq!(
            r.route(key(0x17, true), true),
            KeyAction::Hotkey(Hotkey::ToggleStats)
        );
    }

    #[test]
    fn hotkeys_work_in_view_only_mode() {
        let mut r = KeyRouter::default();
        r.route(key(0x1D, true), false);
        r.route(key(0x38, true), false);
        assert_eq!(
            r.route(key(0x17, true), false),
            KeyAction::Hotkey(Hotkey::ToggleStats)
        );
    }

    #[test]
    fn altgr_is_not_ctrl_alt() {
        // Italian AltGr+Enter seen by the hook: fake LCtrl (0x21D) + Right Alt.
        let mut r = KeyRouter::default();
        assert_eq!(r.route(key(0x21D, true), true), KeyAction::Swallow);
        assert!(matches!(
            r.route(ext(0x38, true), true),
            KeyAction::Forward(_)
        ));
        assert_eq!(r.route(key(0x1C, true), true), forwarded(0x1C, true));
    }

    #[test]
    fn release_of_a_key_pressed_before_us_goes_back_to_windows() {
        // Full screen entered with Ctrl+Alt+Enter: the hook is installed
        // while the three keys are down, and Windows saw their presses.
        let mut r = KeyRouter::default();
        assert_eq!(r.route(key(0x1C, false), true), KeyAction::PassThrough);
        assert_eq!(r.route(key(0x1D, false), true), KeyAction::PassThrough);
        assert_eq!(r.route(key(0x38, false), true), KeyAction::PassThrough);
        // Nor are Ctrl and Alt left "held" for hotkey detection.
        assert_eq!(r.route(key(0x17, true), true), forwarded(0x17, true));
    }

    #[test]
    fn release_after_a_seen_press_is_forwarded_once() {
        let mut r = KeyRouter::default();
        r.route(key(0x2A, true), true);
        r.route(key(0x2A, true), true); // auto-repeat
        assert_eq!(r.route(key(0x2A, false), true), forwarded(0x2A, false));
        assert_eq!(r.route(key(0x2A, false), true), KeyAction::PassThrough);
    }

    #[test]
    fn keys_without_scan_code_are_left_to_windows() {
        let mut r = KeyRouter::default();
        assert_eq!(r.route(key(0, true), true), KeyAction::PassThrough);
    }
}
