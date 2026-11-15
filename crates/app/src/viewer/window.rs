//! The viewer window (winit). It manages the window's lifetime, forwards
//! resizes and local input, and runs the viewer's hotkeys; all video work
//! happens on the render thread.

use std::sync::mpsc::Receiver;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use pipeline::ViewerStats;
use protocol::control::StreamInfo;
use protocol::input::CursorShape;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Fullscreen, Window, WindowId};

use super::Outgoing;
use super::cursor::CursorCache;
use super::key_log;
use super::key_router::Hotkey;
use super::keyboard_hook::KeyboardHook;
use super::remote::RemoteControl;
use super::render_thread::{RenderInput, RenderThread, RttProbe};
use super::toolbar::{BarAction, ControlBar, LEFT};

/// The control bar with the renderer's geometry (the one it draws with).
type Bar = ControlBar<fn((u32, u32), f32, usize, bool) -> render::ToolbarGeometry>;

/// Largest initial window, in logical pixels; the user can resize freely.
const MAX_INITIAL: (f64, f64) = (1280.0, 800.0);

#[derive(Debug)]
pub enum UserEvent {
    HostLeft(String),
    RenderFailed(String),
    Cursor(CursorShape),
    CursorVisible(bool),
    /// The host switched remote control on/off mid-session.
    InputAllowed(bool),
    Hotkey(Hotkey),
}

/// Who may enable remote control: both the host and the viewer's user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlPolicy {
    pub host_allows: bool,
    /// The viewer chose `--view-only`.
    pub view_only: bool,
}

impl ControlPolicy {
    pub fn enabled(self) -> bool {
        self.host_allows && !self.view_only
    }
}

/// How the window starts.
pub struct WindowOptions {
    pub title: String,
    pub fullscreen: bool,
    /// Statistics overlay visible from the start.
    pub stats: bool,
}

/// Inputs consumed when the window opens and the render thread starts.
struct Pending {
    datagrams: Receiver<Bytes>,
    rtt: RttProbe,
}

pub struct ViewerWindow {
    options: WindowOptions,
    stream: StreamInfo,
    pending: Option<Pending>,
    outgoing: Outgoing,
    proxy: EventLoopProxy<UserEvent>,
    policy: ControlPolicy,
    remote: RemoteControl,
    cursors: CursorCache,
    bar: Bar,
    window: Option<Window>,
    /// Installed only in full screen (like Remote Desktop): then Alt+Tab,
    /// the Windows key etc. go to the host; in a window they stay local and
    /// keys come from winit's events. Also `None` if it could not be
    /// installed: then system shortcuts stay local even in full screen.
    hook: Option<KeyboardHook>,
    render: Option<RenderThread>,
    stats: Option<ViewerStats>,
    error: Option<String>,
}

impl ViewerWindow {
    pub fn new(
        options: WindowOptions,
        stream: StreamInfo,
        datagrams: Receiver<Bytes>,
        rtt: RttProbe,
        outgoing: Outgoing,
        policy: ControlPolicy,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        let remote = RemoteControl::new(
            outgoing.clone(),
            policy.enabled(),
            (stream.width, stream.height),
        );
        Self {
            options,
            stream,
            pending: Some(Pending { datagrams, rtt }),
            outgoing,
            proxy,
            policy,
            remote,
            cursors: CursorCache::default(),
            bar: ControlBar::new(render::toolbar_geometry as _),
            window: None,
            hook: None,
            render: None,
            stats: None,
            error: None,
        }
    }

    pub fn stats(&self) -> Option<&ViewerStats> {
        self.stats.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn open(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let (width, height) = initial_size(self.stream.width, self.stream.height);
        let mut attributes = Window::default_attributes()
            .with_title(&self.options.title)
            .with_inner_size(LogicalSize::new(width, height))
            // OLE drag-and-drop would force an STA on this thread; unneeded.
            .with_drag_and_drop(false);
        if self.options.fullscreen {
            attributes = attributes.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = event_loop.create_window(attributes)?;
        let RawWindowHandle::Win32(handle) = window.window_handle()?.as_raw() else {
            bail!("not a Win32 window");
        };
        let hwnd = handle.hwnd.get();
        self.capture_system_keys(self.options.fullscreen);

        let PhysicalSize { width, height } = window.inner_size();
        let Pending { datagrams, rtt } = self.pending.take().context("window opened twice")?;
        let input = RenderInput {
            hwnd,
            stream: self.stream.clone(),
            datagrams,
            outgoing: self.outgoing.clone(),
            rtt,
        };
        let proxy = self.proxy.clone();
        let render = RenderThread::start(input, width, height, move |result| {
            if let Err(e) = result {
                let _ = proxy.send_event(UserEvent::RenderFailed(format!("{e:#}")));
            }
        })?;
        if self.options.stats {
            render.toggle_stats();
        }
        let scale = window.scale_factor() as f32;
        self.bar.set_window((width, height), scale, Instant::now());
        self.bar.set_fullscreen(self.options.fullscreen);
        self.render = Some(render);
        self.window = Some(window);
        self.sync_bar();
        Ok(())
    }

    /// Joins the render thread (it draws into the window) before closing.
    fn close(&mut self, event_loop: &ActiveEventLoop) {
        self.hook = None;
        if let Some(mut render) = self.render.take() {
            match render.stop() {
                Some(Ok(stats)) => self.stats = Some(stats),
                Some(Err(e)) if self.error.is_none() => self.error = Some(format!("{e:#}")),
                _ => {}
            }
        }
        self.window = None;
        event_loop.exit();
    }

    /// Installs (full screen) or removes (window) the keyboard hook that
    /// sends system shortcuts to the host.
    fn capture_system_keys(&mut self, capture: bool) {
        self.remote.reset_keys();
        if !capture {
            self.hook = None;
            return;
        }
        if self.hook.is_none() {
            let proxy = self.proxy.clone();
            self.hook = KeyboardHook::install(
                self.policy.enabled(),
                self.outgoing.clone(),
                Box::new(move |hotkey| {
                    let _ = proxy.send_event(UserEvent::Hotkey(hotkey));
                }),
            )
            .inspect_err(|e| eprintln!("warning: {e:#}; system shortcuts stay local"))
            .ok();
        }
    }

    fn run_hotkey(&mut self, hotkey: Hotkey) {
        match hotkey {
            Hotkey::ToggleFullscreen => {
                let Some(window) = &self.window else { return };
                let entering = window.fullscreen().is_none();
                self.set_fullscreen(entering);
            }
            Hotkey::ToggleStats => self.toggle_stats(),
        }
    }

    fn set_fullscreen(&mut self, on: bool) {
        let Some(window) = &self.window else { return };
        key_log::log(|| format!("window: fullscreen {on}"));
        window.set_fullscreen(on.then_some(Fullscreen::Borderless(None)));
        self.capture_system_keys(on);
        self.bar.set_fullscreen(on);
    }

    fn toggle_stats(&self) {
        if let Some(render) = &self.render {
            render.toggle_stats();
        }
    }

    /// Pushes the control bar's state to the renderer (when changed) and the
    /// cursor: the arrow over the bar, the host's shape elsewhere.
    fn sync_bar(&mut self) {
        let (Some(window), Some(render)) = (&self.window, &self.render) else {
            return;
        };
        if let Some(view) = self.bar.take_changed_view() {
            render.set_toolbar(view);
        }
        self.cursors.set_arrow(window, self.bar.wants_arrow());
    }

    fn run_bar_action(&mut self, event_loop: &ActiveEventLoop, action: BarAction) {
        match action {
            BarAction::LeaveFullscreen => self.set_fullscreen(false),
            BarAction::ToggleStats => self.toggle_stats(),
            BarAction::Disconnect => self.close(event_loop),
        }
    }

    fn update_bar_window(&mut self) {
        if let Some(window) = &self.window {
            let PhysicalSize { width, height } = window.inner_size();
            self.bar.set_window(
                (width, height),
                window.scale_factor() as f32,
                Instant::now(),
            );
        }
    }

    fn set_host_allows(&mut self, allowed: bool) {
        self.policy.host_allows = allowed;
        let enabled = self.policy.enabled();
        self.remote.set_enabled(enabled);
        if let Some(hook) = &self.hook {
            hook.set_forwarding(enabled);
        }
    }
}

impl ApplicationHandler<UserEvent> for ViewerWindow {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(e) = self.open(event_loop)
        {
            self.error = Some(format!("cannot open the window: {e:#}"));
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.close(event_loop),
            WindowEvent::Resized(size) => {
                if let Some(render) = &self.render {
                    render.resize(size.width, size.height);
                }
                self.update_bar_window();
            }
            WindowEvent::ScaleFactorChanged { .. } => self.update_bar_window(),
            // The control bar sees pointer and button events first; what it
            // consumes never reaches the host.
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(window) = &self.window {
                    let size = window.inner_size();
                    let consumed = self
                        .bar
                        .pointer_moved(position.x as i32, position.y as i32, Instant::now())
                        .consumed;
                    if !consumed {
                        self.remote.pointer_moved(position, size);
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => self.bar.pointer_left(Instant::now()),
            WindowEvent::MouseInput { state, button, .. } => {
                let outcome =
                    self.bar
                        .button(button_id(button), state.is_pressed(), Instant::now());
                if !outcome.consumed {
                    self.remote.button(button, state);
                }
                if let Some(action) = outcome.action {
                    self.run_bar_action(event_loop, action);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => self.remote.wheel(delta),
            // A working hook (full screen) swallows every key it handles, so
            // those never reach winit; whatever does arrive here was not
            // handled by a hook and must be routed now. This also keeps the
            // hotkeys working if the hook is installed but receives nothing.
            // Synthetic events are winit replaying keys held across focus
            // changes; ReleaseAll on focus loss covers them.
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => {
                key_log::log(|| format!("winit key {:?} {:?}", event.physical_key, event.state));
                if let Some(hotkey) = self.remote.key(event.physical_key, event.state) {
                    self.run_hotkey(hotkey);
                }
            }
            WindowEvent::Focused(focused) => {
                key_log::log(|| format!("window: focused {focused}"));
                if !focused {
                    self.remote.focus_lost();
                }
            }
            _ => {}
        }
        self.sync_bar();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Cursor(shape) => {
                if let Some(window) = &self.window {
                    self.cursors.apply(event_loop, window, shape);
                }
            }
            UserEvent::CursorVisible(visible) => {
                if let Some(window) = &self.window {
                    self.cursors.set_host_visible(window, visible);
                }
            }
            UserEvent::InputAllowed(allowed) => self.set_host_allows(allowed),
            UserEvent::Hotkey(hotkey) => self.run_hotkey(hotkey),
            UserEvent::HostLeft(reason) => {
                println!("{reason}");
                self.close(event_loop);
            }
            UserEvent::RenderFailed(reason) => {
                self.error = Some(reason);
                self.close(event_loop);
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.bar.tick(Instant::now());
        self.sync_bar();
        event_loop.set_control_flow(match self.bar.next_deadline() {
            Some(deadline) => ControlFlow::WaitUntil(deadline),
            None => ControlFlow::Wait,
        });
    }
}

/// Stable small id per mouse button for the control bar.
fn button_id(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => LEFT,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::Back => 3,
        MouseButton::Forward => 4,
        MouseButton::Other(n) => 5u8.saturating_add(n.min(200) as u8),
    }
}

/// Stream size scaled down (never up) to fit [`MAX_INITIAL`], keeping the
/// aspect ratio. Pure.
fn initial_size(width: u32, height: u32) -> (f64, f64) {
    let (w, h) = (f64::from(width.max(1)), f64::from(height.max(1)));
    let scale = (MAX_INITIAL.0 / w).min(MAX_INITIAL.1 / h).min(1.0);
    ((w * scale).round(), (h * scale).round())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_streams_are_scaled_down_keeping_aspect() {
        assert_eq!(initial_size(1920, 1080), (1280.0, 720.0));
        assert_eq!(initial_size(2560, 1600), (1280.0, 800.0));
        assert_eq!(initial_size(1080, 1920), (450.0, 800.0));
    }

    #[test]
    fn small_streams_are_not_enlarged() {
        assert_eq!(initial_size(800, 600), (800.0, 600.0));
    }

    #[test]
    fn degenerate_size_does_not_divide_by_zero() {
        let (w, h) = initial_size(0, 0);
        assert!(w > 0.0 && h > 0.0);
    }

    #[test]
    fn control_needs_host_permission_and_viewer_consent() {
        let policy = |host_allows, view_only| ControlPolicy {
            host_allows,
            view_only,
        };
        assert!(policy(true, false).enabled());
        assert!(!policy(false, false).enabled());
        assert!(!policy(true, true).enabled());
    }
}
