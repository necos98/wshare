//! Forwards the viewer's mouse and keyboard to the host.

use protocol::control::ControlMessage;
use protocol::input::InputEvent;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta};
use winit::keyboard::PhysicalKey;

use super::Outgoing;
use super::input_map::{map_button, map_key, map_wheel, normalize_pointer};

pub struct RemoteControl {
    outgoing: Outgoing,
    /// False for view-only sessions: nothing is sent.
    enabled: bool,
    stream: (u32, u32),
    /// Last position sent, to drop moves that land on the same pixel.
    last_position: Option<(f32, f32)>,
}

impl RemoteControl {
    pub fn new(outgoing: Outgoing, enabled: bool, stream: (u32, u32)) -> Self {
        Self { outgoing, enabled, stream, last_position: None }
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
            self.send(InputEvent::MouseButton { button, pressed: state.is_pressed() });
        }
    }

    pub fn wheel(&mut self, delta: MouseScrollDelta) {
        if let Some(event) = map_wheel(delta) {
            self.send(event);
        }
    }

    /// Auto-repeats are forwarded: Windows does not repeat injected keys.
    pub fn key(&mut self, key: PhysicalKey, state: ElementState) {
        if let Some(event) = map_key(key, state) {
            self.send(event);
        }
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
        remote.pointer_moved(PhysicalPosition::new(50.0, 50.0), PhysicalSize::new(100, 100));
        remote.button(MouseButton::Left, ElementState::Pressed);
        remote.key(PhysicalKey::Code(KeyCode::KeyA), ElementState::Pressed);
        remote.focus_lost();
        let events = received(&mut rx);
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], InputEvent::MouseMove { .. }));
        assert_eq!(events[3], InputEvent::ReleaseAll);
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
    fn view_only_sends_nothing() {
        let (tx, mut rx) = unbounded_channel();
        let mut remote = RemoteControl::new(tx, false, (100, 100));
        remote.button(MouseButton::Left, ElementState::Pressed);
        remote.focus_lost();
        assert!(received(&mut rx).is_empty());
    }
}
