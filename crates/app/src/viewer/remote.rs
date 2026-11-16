//! Forwards the viewer's mouse and keyboard to the host.

use protocol::control::ControlMessage;
use protocol::input::InputEvent;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta};
use winit::keyboard::PhysicalKey;

use super::Outgoing;
use super::input_map::{map_button, map_wheel, normalize_pointer, raw_key};
use super::key_router::{Hotkey, KeyAction, KeyRouter};

pub struct RemoteControl {
    outgoing: Outgoing,
    /// False for view-only sessions: nothing is sent.
    enabled: bool,
    stream: (u32, u32),
    /// Last position sent, to drop moves that land on the same pixel.
    last_position: Option<(f32, f32)>,
    /// Routes the window's own key events (used when no keyboard hook is
    /// installed, i.e. outside full screen).
    keys: KeyRouter,
}

impl RemoteControl {
    pub fn new(outgoing: Outgoing, enabled: bool, stream: (u32, u32)) -> Self {
        Self {
            outgoing,
            enabled,
            stream,
            last_position: None,
            keys: KeyRouter::default(),
        }
    }

    /// Remote control switched on/off mid-session. Turning it off releases
    /// whatever is held on the host first.
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled && !enabled {
            self.send(InputEvent::ReleaseAll);
        }
        self.enabled = enabled;
        self.last_position = None;
    }

    /// The host streams a picture of `stream` size from now on (another
    /// monitor or resolution): positions map onto it. Held keys are
    /// forgotten, as the host lets go of them when it changes monitor.
    pub fn set_stream(&mut self, stream: (u32, u32)) {
        self.stream = stream;
        self.last_position = None;
        self.reset_keys();
    }

    pub fn pointer_moved(&mut self, position: PhysicalPosition<f64>, window: PhysicalSize<u32>) {
        let window = (window.width, window.height);
        let Some(normalized) = normalize_pointer((position.x, position.y), window, self.stream)
        else {
            return;
        };
        if self.last_position != Some(normalized) {
            self.last_position = Some(normalized);
            let (x, y) = normalized;
            self.send(InputEvent::MouseMove { x, y });
        }
    }

    pub fn button(&mut self, button: MouseButton, state: ElementState) {
        if let Some(button) = map_button(button) {
            self.send(InputEvent::MouseButton {
                button,
                pressed: state.is_pressed(),
            });
        }
    }

    pub fn wheel(&mut self, delta: MouseScrollDelta) {
        if let Some(event) = map_wheel(delta) {
            self.send(event);
        }
    }

    /// A key event from the window. Returns the viewer hotkey it completes,
    /// to run locally. Auto-repeats are forwarded: Windows does not repeat
    /// injected keys.
    pub fn key(&mut self, key: PhysicalKey, state: ElementState) -> Option<Hotkey> {
        match self.keys.route(raw_key(key, state)?, self.enabled) {
            KeyAction::Hotkey(hotkey) => {
                // Ctrl and Alt already went to the host: release them there.
                self.send(InputEvent::ReleaseAll);
                Some(hotkey)
            }
            KeyAction::Forward(event) => {
                self.send(event);
                None
            }
            KeyAction::Swallow | KeyAction::PassThrough => None,
        }
    }

    /// Forget modifier state: keys switch between the window and the hook
    /// (entering/leaving full screen), so held keys may change source.
    pub fn reset_keys(&mut self) {
        self.keys = KeyRouter::default();
    }

    /// The window lost focus: we will not see the releases of keys held now.
    pub fn focus_lost(&mut self) {
        self.send(InputEvent::ReleaseAll);
    }

    fn send(&self, event: InputEvent) {
        if self.enabled {
            // Fails only once the session is over; nothing left to do then.
            let _ = self.outgoing.send(ControlMessage::Input(event));
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::unbounded_channel;
    use winit::keyboard::KeyCode;

    use super::*;

    fn received(rx: &mut tokio::sync::mpsc::UnboundedReceiver<ControlMessage>) -> Vec<InputEvent> {
        let mut events = Vec::new();
        while let Ok(ControlMessage::Input(event)) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    #[test]
    fn forwards_input_when_enabled() {
        let (tx, mut rx) = unbounded_channel();
        let mut remote = RemoteControl::new(tx, true, (100, 100));
        remote.pointer_moved(
            PhysicalPosition::new(50.0, 50.0),
            PhysicalSize::new(100, 100),
        );
        remote.button(MouseButton::Left, ElementState::Pressed);
        remote.key(PhysicalKey::Code(KeyCode::KeyA), ElementState::Pressed);
        remote.focus_lost();
        let events = received(&mut rx);
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], InputEvent::MouseMove { .. }));
        assert_eq!(events[3], InputEvent::ReleaseAll);
    }

    #[test]
    fn a_new_stream_size_remaps_the_pointer() {
        let (tx, mut rx) = unbounded_channel();
        let mut remote = RemoteControl::new(tx, true, (100, 100));
        let window = PhysicalSize::new(200, 100);
        // A square stream in a wide window: bars left and right.
        remote.pointer_moved(PhysicalPosition::new(50.0, 50.0), window);
        remote.set_stream((200, 100));
        // Same spot: now it is a quarter of the way across, and sent again.
        remote.pointer_moved(PhysicalPosition::new(50.0, 50.0), window);
        let events = received(&mut rx);
        let [
            InputEvent::MouseMove { x: before, .. },
            InputEvent::MouseMove { x: after, .. },
        ] = events[..]
        else {
            panic!("{events:?}")
        };
        assert!(
            before < 0.01,
            "in the left bar: the left edge, got {before}"
        );
        assert!((after - 0.2525).abs() < 0.001, "got {after}");
    }

    #[test]
    fn moves_within_the_same_pixel_are_sent_once() {
        let (tx, mut rx) = unbounded_channel();
        let mut remote = RemoteControl::new(tx, true, (100, 100));
        let size = PhysicalSize::new(100, 100);
        remote.pointer_moved(PhysicalPosition::new(10.2, 10.2), size);
        remote.pointer_moved(PhysicalPosition::new(10.7, 10.9), size);
        remote.pointer_moved(PhysicalPosition::new(11.0, 10.0), size);
        assert_eq!(received(&mut rx).len(), 2);
    }

    #[test]
    fn enabling_mid_session_starts_forwarding_and_disabling_releases_keys() {
        let (tx, mut rx) = unbounded_channel();
        let mut remote = RemoteControl::new(tx, false, (100, 100));
        remote.button(MouseButton::Left, ElementState::Pressed);
        assert!(received(&mut rx).is_empty());
        remote.set_enabled(true);
        remote.button(MouseButton::Left, ElementState::Pressed);
        remote.set_enabled(false);
        remote.button(MouseButton::Left, ElementState::Released);
        let events = received(&mut rx);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1], InputEvent::ReleaseAll);
    }

    #[test]
    fn window_keys_can_trigger_hotkeys_and_release_modifiers_on_host() {
        let (tx, mut rx) = unbounded_channel();
        let mut remote = RemoteControl::new(tx, true, (100, 100));
        let press = |r: &mut RemoteControl, k| r.key(PhysicalKey::Code(k), ElementState::Pressed);
        assert_eq!(press(&mut remote, KeyCode::ControlLeft), None);
        assert_eq!(press(&mut remote, KeyCode::AltLeft), None);
        assert_eq!(
            press(&mut remote, KeyCode::Enter),
            Some(Hotkey::ToggleFullscreen)
        );
        let events = received(&mut rx);
        assert_eq!(events.len(), 3); // Ctrl, Alt, then ReleaseAll; Enter not sent
        assert_eq!(events[2], InputEvent::ReleaseAll);
    }

    #[test]
    fn hotkeys_work_in_view_only_without_sending_anything() {
        let (tx, mut rx) = unbounded_channel();
        let mut remote = RemoteControl::new(tx, false, (100, 100));
        remote.key(
            PhysicalKey::Code(KeyCode::ControlRight),
            ElementState::Pressed,
        );
        remote.key(PhysicalKey::Code(KeyCode::AltRight), ElementState::Pressed);
        let hotkey = remote.key(PhysicalKey::Code(KeyCode::KeyI), ElementState::Pressed);
        assert_eq!(hotkey, Some(Hotkey::ToggleStats));
        assert!(received(&mut rx).is_empty());
    }

    #[test]
    fn view_only_sends_nothing() {
        let (tx, mut rx) = unbounded_channel();
        let mut remote = RemoteControl::new(tx, false, (100, 100));
        remote.button(MouseButton::Left, ElementState::Pressed);
        remote.focus_lost();
        assert!(received(&mut rx).is_empty());
    }
}
