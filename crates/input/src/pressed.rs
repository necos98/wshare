//! What the remote user currently holds down on the host.
//!
//! The viewer cannot always deliver releases (it loses focus, the session
//! drops), and a key or button left down on the host is a nasty failure: a
//! stuck Ctrl turns every local click into Ctrl+click. This set lets the
//! facade release everything on demand.
//!
//! It is a deliberate over-approximation of the real state: entries are
//! added when a press is *attempted* and removed only once the release was
//! injected. A spare release is harmless; a missing one is not.

use protocol::input::MouseButton;

use crate::keys::Scancode;
use crate::synth::SyntheticInput;

/// Something that can be held down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Held {
    Key(Scancode),
    Button(MouseButton),
}

impl Held {
    /// The event that releases it.
    pub fn release(self) -> SyntheticInput {
        match self {
            Self::Key(key) => SyntheticInput::key(key, false),
            Self::Button(button) => SyntheticInput::button(button, false),
        }
    }
}

/// Held keys and buttons, in press order (a handful at most, so a `Vec`).
#[derive(Debug, Default, Clone)]
pub struct PressedState {
    held: Vec<Held>,
}

impl PressedState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a press. Returns `false` if it was already held.
    pub fn press(&mut self, item: Held) -> bool {
        if self.is_held(item) {
            return false;
        }
        self.held.push(item);
        true
    }

    /// Forgets a held item. Returns `false` if it was not held.
    pub fn release(&mut self, item: Held) -> bool {
        let before = self.held.len();
        self.held.retain(|&h| h != item);
        self.held.len() != before
    }

    pub fn is_held(&self, item: Held) -> bool {
        self.held.contains(&item)
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Items in the order [`Self::releases`] releases them (most recent
    /// first, so modifiers pressed first are released last, like a human).
    pub fn release_order(&self) -> impl Iterator<Item = Held> + '_ {
        self.held.iter().rev().copied()
    }

    /// The events releasing everything held, in [`Self::release_order`].
    pub fn releases(&self) -> Vec<SyntheticInput> {
        self.release_order().map(Held::release).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: u16, extended: bool) -> Held {
        Held::Key(Scancode::new(code, extended).unwrap())
    }

    #[test]
    fn press_is_recorded_once() {
        let mut s = PressedState::new();
        assert!(s.press(key(0x1D, false)));
        assert!(!s.press(key(0x1D, false)));
        assert!(s.press(key(0x1D, true)), "right Ctrl is a different key");
        assert_eq!(s.releases().len(), 2);
    }

    #[test]
    fn release_of_unheld_is_reported() {
        let mut s = PressedState::new();
        assert!(!s.release(Held::Button(MouseButton::Left)));
        s.press(Held::Button(MouseButton::Left));
        assert!(s.release(Held::Button(MouseButton::Left)));
        assert!(s.is_empty());
    }

    #[test]
    fn releases_in_reverse_press_order() {
        let mut s = PressedState::new();
        let ctrl = key(0x1D, false);
        let c = key(0x2E, false);
        s.press(ctrl);
        s.press(c);
        s.press(Held::Button(MouseButton::Back));
        assert_eq!(
            s.releases(),
            vec![
                Held::Button(MouseButton::Back).release(),
                c.release(),
                ctrl.release()
            ]
        );
    }
}
