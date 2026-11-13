//! The facade: applies protocol events to the host desktop.

use anyhow::Result;
use protocol::input::{InputEvent, MouseButton};

use crate::backend::{InputBackend, SendInputBackend};
use crate::geometry::{ScreenArea, to_absolute};
use crate::keys::Scancode;
use crate::metrics::{DesktopMetrics, SystemMetrics};
use crate::pressed::{Held, PressedState};
use crate::synth::SyntheticInput;

/// What [`RemoteInput::handle`] did with an event. Only backend failures
/// are `Err`; everything here is routine and non-fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Every resulting event was injected.
    Injected,
    /// Nothing was injected, on purpose.
    Ignored(Ignored),
    /// Windows injected fewer events than asked: UIPI (elevated window in
    /// the foreground) or the secure desktop (UAC, lock screen). Expected
    /// to persist for a while; `first` is true only on the first blocked
    /// event since the last successful one, so the app can log once.
    Blocked { first: bool },
}

/// Why an event was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ignored {
    /// NaN/infinite coordinates, or an empty monitor/desktop rectangle.
    InvalidPosition,
    /// Scan code rejected by [`Scancode::new`].
    InvalidScancode,
    /// Press of a mouse button that is already down (a second "down" would
    /// confuse click/drag detection).
    AlreadyHeld,
    /// Release of a key/button the remote user does not hold (e.g. already
    /// released by `ReleaseAll`): not ours to release.
    NotHeld,
    /// Wheel event with no rotation.
    NoMotion,
}

/// Injects the remote user's input into the host desktop, restricted to the
/// shared monitor, and guarantees nothing stays pressed: `ReleaseAll` and
/// `Drop` release every key and button the remote user holds.
///
/// Key repeats (`Key { pressed: true }` for a held key) are injected each
/// time: Windows does not auto-repeat injected keys, the viewer forwards
/// its own repeats.
pub struct RemoteInput<B: InputBackend = SendInputBackend, M: DesktopMetrics = SystemMetrics> {
    area: ScreenArea,
    backend: B,
    metrics: M,
    pressed: PressedState,
    blocked: bool,
}

impl RemoteInput {
    /// Real `SendInput` injection onto `area`, the shared monitor's
    /// rectangle in virtual-desktop pixels.
    pub fn new(area: ScreenArea) -> Self {
        Self::with_backend(area, SendInputBackend::new(), SystemMetrics)
    }
}

impl<B: InputBackend, M: DesktopMetrics> RemoteInput<B, M> {
    pub fn with_backend(area: ScreenArea, backend: B, metrics: M) -> Self {
        Self {
            area,
            backend,
            metrics,
            pressed: PressedState::new(),
            blocked: false,
        }
    }

    /// The shared monitor moved or changed (resolution, other monitor).
    pub fn set_area(&mut self, area: ScreenArea) {
        self.area = area;
    }

    pub fn area(&self) -> ScreenArea {
        self.area
    }

    /// Applies one event received from the viewer.
    pub fn handle(&mut self, event: &InputEvent) -> Result<Outcome> {
        match *event {
            InputEvent::MouseMove { x, y } => self.move_to(x, y),
            InputEvent::MouseButton { button, pressed } => self.button(button, pressed),
            InputEvent::MouseWheel { delta_x, delta_y } => self.wheel(delta_x, delta_y),
            InputEvent::Key {
                scancode,
                extended,
                pressed,
            } => self.key(scancode, extended, pressed),
            InputEvent::ReleaseAll => self.release_all(),
        }
    }

    /// Releases every key and button the remote user holds.
    pub fn release_all(&mut self) -> Result<Outcome> {
        let releases = self.pressed.releases();
        if releases.is_empty() {
            return Ok(Outcome::Ignored(Ignored::NotHeld));
        }
        let sent = self.backend.send(&releases)?;
        // Forget only what was actually released; the rest is retried by the
        // next ReleaseAll / release / drop.
        let done: Vec<Held> = self.pressed.release_order().take(sent as usize).collect();
        for item in done {
            self.pressed.release(item);
        }
        Ok(self.outcome(sent, releases.len()))
    }

    fn move_to(&mut self, x: f32, y: f32) -> Result<Outcome> {
        let desktop = self.metrics.virtual_desktop();
        match to_absolute(x, y, &self.area, &desktop) {
            Some(point) => self.send(&[SyntheticInput::move_to(point)]),
            None => Ok(Outcome::Ignored(Ignored::InvalidPosition)),
        }
    }

    fn button(&mut self, button: MouseButton, pressed: bool) -> Result<Outcome> {
        let item = Held::Button(button);
        if !pressed {
            return self.release(item);
        }
        if !self.pressed.press(item) {
            return Ok(Outcome::Ignored(Ignored::AlreadyHeld));
        }
        self.send(&[SyntheticInput::button(button, true)])
    }

    fn wheel(&mut self, delta_x: i32, delta_y: i32) -> Result<Outcome> {
        let inputs = SyntheticInput::wheel(delta_x, delta_y);
        if inputs.is_empty() {
            return Ok(Outcome::Ignored(Ignored::NoMotion));
        }
        self.send(&inputs)
    }

    fn key(&mut self, scancode: u16, extended: bool, pressed: bool) -> Result<Outcome> {
        let Some(key) = Scancode::new(scancode, extended) else {
            return Ok(Outcome::Ignored(Ignored::InvalidScancode));
        };
        if !pressed {
            return self.release(Held::Key(key));
        }
        // Repeats are injected too (see the type docs); recorded once.
        self.pressed.press(Held::Key(key));
        self.send(&[SyntheticInput::key(key, true)])
    }

    fn release(&mut self, item: Held) -> Result<Outcome> {
        if !self.pressed.is_held(item) {
            return Ok(Outcome::Ignored(Ignored::NotHeld));
        }
        let outcome = self.send(&[item.release()])?;
        if outcome == Outcome::Injected {
            self.pressed.release(item);
        }
        Ok(outcome)
    }

    fn send(&mut self, inputs: &[SyntheticInput]) -> Result<Outcome> {
        let sent = self.backend.send(inputs)?;
        Ok(self.outcome(sent, inputs.len()))
    }

    /// Classifies a send and maintains the "log once" latch.
    fn outcome(&mut self, sent: u32, expected: usize) -> Outcome {
        if sent as usize >= expected {
            self.blocked = false;
            return Outcome::Injected;
        }
        let first = !self.blocked;
        self.blocked = true;
        Outcome::Blocked { first }
    }
}

impl<B: InputBackend, M: DesktopMetrics> Drop for RemoteInput<B, M> {
    /// Session over: never leave a key or button down on the host.
    fn drop(&mut self) {
        // Nothing useful to do with an error while dropping.
        let _ = self.release_all();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use anyhow::anyhow;
    use protocol::input::MouseButton::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_HWHEEL,
        MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_WHEEL,
    };

    use super::*;
    use crate::geometry::AbsolutePoint;

    /// Records what would be injected; accepts at most `accept` events per
    /// call (simulates UIPI blocking), or fails when `fail` is set.
    #[derive(Clone, Default)]
    struct Recorder {
        sent: Rc<RefCell<Vec<SyntheticInput>>>,
        accept: Rc<RefCell<Option<u32>>>,
        fail: Rc<RefCell<bool>>,
    }

    impl Recorder {
        fn take(&self) -> Vec<SyntheticInput> {
            self.sent.borrow_mut().drain(..).collect()
        }
        fn block(&self, accept: Option<u32>) {
            *self.accept.borrow_mut() = accept;
        }
    }

    impl InputBackend for Recorder {
        fn send(&mut self, inputs: &[SyntheticInput]) -> Result<u32> {
            if *self.fail.borrow() {
                return Err(anyhow!("backend down"));
            }
            let n = self
                .accept
                .borrow()
                .map_or(inputs.len(), |a| inputs.len().min(a as usize));
            self.sent.borrow_mut().extend_from_slice(&inputs[..n]);
            Ok(n as u32)
        }
    }

    const PRIMARY: ScreenArea = ScreenArea::new(0, 0, 1920, 1080);

    fn remote(
        area: ScreenArea,
        desktop: ScreenArea,
    ) -> (RemoteInput<Recorder, ScreenArea>, Recorder) {
        let rec = Recorder::default();
        (RemoteInput::with_backend(area, rec.clone(), desktop), rec)
    }

    fn key(scancode: u16, extended: bool, pressed: bool) -> InputEvent {
        InputEvent::Key {
            scancode,
            extended,
            pressed,
        }
    }

    fn btn(button: MouseButton, pressed: bool) -> InputEvent {
        InputEvent::MouseButton { button, pressed }
    }

    fn kb(
        scan: u16,
        flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS,
    ) -> SyntheticInput {
        SyntheticInput::Keyboard { scan, flags }
    }

    fn moved_to(sent: &[SyntheticInput]) -> AbsolutePoint {
        match sent {
            [SyntheticInput::Mouse { dx, dy, .. }] => AbsolutePoint { x: *dx, y: *dy },
            other => panic!("expected one move, got {other:?}"),
        }
    }

    #[test]
    fn remote_input_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<RemoteInput>();
    }

    #[test]
    fn move_uses_area_and_live_desktop() {
        let right = ScreenArea::new(1920, 0, 1920, 1080);
        let desktop = ScreenArea::new(0, 0, 3840, 1080);
        let (mut r, rec) = remote(right, desktop);
        assert_eq!(
            r.handle(&InputEvent::MouseMove { x: 0.0, y: 0.0 }).unwrap(),
            Outcome::Injected
        );
        let p = moved_to(&rec.take());
        assert_eq!(p, to_absolute(0.0, 0.0, &right, &desktop).unwrap());
        assert!(
            (32768..32800).contains(&p.x),
            "left edge of the right half: {p:?}"
        );
    }

    #[test]
    fn move_on_negative_origin_monitor() {
        let left = ScreenArea::new(-1920, 0, 1920, 1080);
        let desktop = ScreenArea::new(-1920, 0, 3840, 1080);
        let (mut r, rec) = remote(left, desktop);
        r.handle(&InputEvent::MouseMove { x: 1.0, y: 1.0 }).unwrap();
        let p = moved_to(&rec.take());
        // Last pixel of the left monitor is desktop pixel 1919 of 3840.
        assert_eq!((i64::from(p.x) * 3840) >> 16, 1919);
        assert_eq!((i64::from(p.y) * 1080) >> 16, 1079);
    }

    #[test]
    fn set_area_retargets_moves() {
        let desktop = ScreenArea::new(0, 0, 3840, 1080);
        let (mut r, rec) = remote(PRIMARY, desktop);
        r.set_area(ScreenArea::new(1920, 0, 1920, 1080));
        r.handle(&InputEvent::MouseMove { x: 0.0, y: 0.0 }).unwrap();
        assert!(moved_to(&rec.take()).x >= 32768);
    }

    #[test]
    fn invalid_moves_are_ignored() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        let out = r
            .handle(&InputEvent::MouseMove {
                x: f32::NAN,
                y: 0.5,
            })
            .unwrap();
        assert_eq!(out, Outcome::Ignored(Ignored::InvalidPosition));
        let (mut r2, rec2) = remote(PRIMARY, ScreenArea::default());
        let out = r2
            .handle(&InputEvent::MouseMove { x: 0.5, y: 0.5 })
            .unwrap();
        assert_eq!(out, Outcome::Ignored(Ignored::InvalidPosition));
        assert!(rec.take().is_empty() && rec2.take().is_empty());
    }

    #[test]
    fn every_button_press_and_release() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        for b in [Left, Right, Middle, Back, Forward] {
            assert_eq!(r.handle(&btn(b, true)).unwrap(), Outcome::Injected);
            assert_eq!(r.handle(&btn(b, false)).unwrap(), Outcome::Injected);
            assert_eq!(
                rec.take(),
                vec![
                    SyntheticInput::button(b, true),
                    SyntheticInput::button(b, false)
                ]
            );
        }
    }

    #[test]
    fn redundant_button_events_are_dropped() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        assert_eq!(
            r.handle(&btn(Left, false)).unwrap(),
            Outcome::Ignored(Ignored::NotHeld)
        );
        r.handle(&btn(Left, true)).unwrap();
        assert_eq!(
            r.handle(&btn(Left, true)).unwrap(),
            Outcome::Ignored(Ignored::AlreadyHeld)
        );
        r.handle(&btn(Left, false)).unwrap();
        assert_eq!(
            r.handle(&btn(Left, false)).unwrap(),
            Outcome::Ignored(Ignored::NotHeld)
        );
        let sent = rec.take();
        assert_eq!(sent.len(), 2);
        assert!(
            matches!(sent[0], SyntheticInput::Mouse { flags, .. } if flags == MOUSEEVENTF_LEFTDOWN)
        );
        assert!(
            matches!(sent[1], SyntheticInput::Mouse { flags, .. } if flags == MOUSEEVENTF_LEFTUP)
        );
    }

    #[test]
    fn wheel_both_axes_and_zero() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        assert_eq!(
            r.handle(&InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: 0
            })
            .unwrap(),
            Outcome::Ignored(Ignored::NoMotion)
        );
        r.handle(&InputEvent::MouseWheel {
            delta_x: 60,
            delta_y: -120,
        })
        .unwrap();
        assert_eq!(
            rec.take(),
            vec![
                SyntheticInput::Mouse {
                    dx: 0,
                    dy: 0,
                    data: (-120i32) as u32,
                    flags: MOUSEEVENTF_WHEEL
                },
                SyntheticInput::Mouse {
                    dx: 0,
                    dy: 0,
                    data: 60,
                    flags: MOUSEEVENTF_HWHEEL
                },
            ]
        );
    }

    #[test]
    fn keys_plain_and_extended() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        r.handle(&key(0x1E, false, true)).unwrap();
        r.handle(&key(0x1E, false, false)).unwrap();
        r.handle(&key(0x4B, true, true)).unwrap(); // Left arrow
        r.handle(&key(0x4B, true, false)).unwrap();
        assert_eq!(
            rec.take(),
            vec![
                kb(0x1E, KEYEVENTF_SCANCODE),
                kb(0x1E, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP),
                kb(0x4B, KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY),
                kb(
                    0x4B,
                    KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP
                ),
            ]
        );
    }

    #[test]
    fn key_repeats_are_injected_but_released_once() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        for _ in 0..3 {
            assert_eq!(
                r.handle(&key(0x0E, false, true)).unwrap(),
                Outcome::Injected
            ); // Backspace
        }
        assert_eq!(rec.take(), vec![kb(0x0E, KEYEVENTF_SCANCODE); 3]);
        r.handle(&InputEvent::ReleaseAll).unwrap();
        assert_eq!(
            rec.take(),
            vec![kb(0x0E, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP)]
        );
    }

    #[test]
    fn release_of_unheld_key_is_dropped() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        assert_eq!(
            r.handle(&key(0x1D, false, false)).unwrap(),
            Outcome::Ignored(Ignored::NotHeld)
        );
        // Left and right Ctrl are distinct keys.
        r.handle(&key(0x1D, false, true)).unwrap();
        assert_eq!(
            r.handle(&key(0x1D, true, false)).unwrap(),
            Outcome::Ignored(Ignored::NotHeld)
        );
        assert_eq!(rec.take(), vec![kb(0x1D, KEYEVENTF_SCANCODE)]);
    }

    #[test]
    fn invalid_scancodes_are_ignored() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        for bad in [0x0000, 0x00E0, 0x0100, 0xE11D] {
            assert_eq!(
                r.handle(&key(bad, false, true)).unwrap(),
                Outcome::Ignored(Ignored::InvalidScancode)
            );
        }
        assert!(rec.take().is_empty());
    }

    #[test]
    fn release_all_releases_everything_in_reverse_order() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        r.handle(&key(0x1D, false, true)).unwrap(); // Ctrl
        r.handle(&key(0x38, true, true)).unwrap(); // AltGr
        r.handle(&btn(Right, true)).unwrap();
        rec.take();
        assert_eq!(
            r.handle(&InputEvent::ReleaseAll).unwrap(),
            Outcome::Injected
        );
        assert_eq!(
            rec.take(),
            vec![
                SyntheticInput::button(Right, false),
                kb(
                    0x38,
                    KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP
                ),
                kb(0x1D, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP),
            ]
        );
        // Nothing left; later releases from the viewer are ignored.
        assert_eq!(
            r.handle(&InputEvent::ReleaseAll).unwrap(),
            Outcome::Ignored(Ignored::NotHeld)
        );
        assert_eq!(
            r.handle(&key(0x1D, false, false)).unwrap(),
            Outcome::Ignored(Ignored::NotHeld)
        );
        assert!(rec.take().is_empty());
    }

    #[test]
    fn drop_releases_held_input() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        r.handle(&key(0x2A, false, true)).unwrap(); // Shift
        r.handle(&btn(Left, true)).unwrap();
        rec.take();
        drop(r);
        assert_eq!(
            rec.take(),
            vec![
                SyntheticInput::button(Left, false),
                kb(0x2A, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP)
            ]
        );
    }

    #[test]
    fn drop_with_nothing_held_sends_nothing() {
        let (r, rec) = remote(PRIMARY, PRIMARY);
        drop(r);
        assert!(rec.take().is_empty());
    }

    #[test]
    fn blocked_is_reported_once_until_recovery() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        rec.block(Some(0));
        let mv = InputEvent::MouseMove { x: 0.5, y: 0.5 };
        assert_eq!(r.handle(&mv).unwrap(), Outcome::Blocked { first: true });
        assert_eq!(r.handle(&mv).unwrap(), Outcome::Blocked { first: false });
        rec.block(None);
        assert_eq!(r.handle(&mv).unwrap(), Outcome::Injected);
        rec.block(Some(0));
        assert_eq!(r.handle(&mv).unwrap(), Outcome::Blocked { first: true });
    }

    #[test]
    fn blocked_release_is_retried_later() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        r.handle(&key(0x1D, false, true)).unwrap();
        rec.take();
        rec.block(Some(0));
        assert_eq!(
            r.handle(&key(0x1D, false, false)).unwrap(),
            Outcome::Blocked { first: true }
        );
        rec.block(None);
        drop(r);
        assert_eq!(
            rec.take(),
            vec![kb(0x1D, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP)]
        );
    }

    #[test]
    fn partial_release_all_keeps_the_rest() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        r.handle(&key(0x1D, false, true)).unwrap();
        r.handle(&key(0x2A, false, true)).unwrap();
        rec.take();
        rec.block(Some(1));
        assert_eq!(
            r.handle(&InputEvent::ReleaseAll).unwrap(),
            Outcome::Blocked { first: true }
        );
        assert_eq!(
            rec.take(),
            vec![kb(0x2A, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP)]
        );
        rec.block(None);
        assert_eq!(
            r.handle(&InputEvent::ReleaseAll).unwrap(),
            Outcome::Injected
        );
        assert_eq!(
            rec.take(),
            vec![kb(0x1D, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP)]
        );
    }

    #[test]
    fn backend_errors_propagate() {
        let (mut r, rec) = remote(PRIMARY, PRIMARY);
        *rec.fail.borrow_mut() = true;
        assert!(r.handle(&InputEvent::MouseMove { x: 0.5, y: 0.5 }).is_err());
    }
}
