//! The viewer window (winit). It manages the window's lifetime, forwards
//! resizes and local input, and runs the viewer's hotkeys; all video work
//! happens on the render thread.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use pipeline::ViewerStats;
use protocol::control::{ControlMessage, StreamInfo};
use protocol::input::CursorShape;
use protocol::session::MonitorEntry;
use tokio::sync::mpsc::UnboundedSender;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Fullscreen, Window, WindowId};

use super::Outgoing;
use super::control_policy::ControlPolicy;
use super::cursor::CursorCache;
use super::failure::ViewerFailure;
use super::feed::VideoIn;
use super::key_log;
use super::key_router::Hotkey;
use super::keyboard_hook::KeyboardHook;
use super::remote::RemoteControl;
use super::render_thread::{RenderInput, RenderThread};
use super::stats::LinkProbe;
use super::toolbar::{BarAction, ControlBar, LEFT};
use super::window_files::WindowFiles;
use crate::desktop::{self, Choice};
use crate::text;
use crate::transfer::{TransferCommand, TransferEvent};

/// The control bar with the renderer's geometry (the one it draws with).
type Bar = ControlBar<fn((u32, u32), f32, usize, bool) -> render::ToolbarGeometry>;

/// Largest initial window, in logical pixels; the user can resize freely.
const MAX_INITIAL: (f64, f64) = (1280.0, 800.0);

#[derive(Debug)]
pub enum UserEvent {
    /// The connection dropped; the viewer is trying to get it back.
    Reconnecting,
    /// The connection is back: a new session follows the old one.
    Reconnected,
    HostLeft(ViewerFailure),
    RenderFailed(String),
    Cursor(CursorShape),
    CursorVisible(bool),
    /// The host switched remote control on/off mid-session.
    InputAllowed(bool),
    /// The host's monitors and the one it shares.
    Monitors {
        monitors: Vec<MonitorEntry>,
        current: u32,
    },
    /// The host started its stream over: the video is `StreamInfo` now.
    StreamChanged(StreamInfo),
    Hotkey(Hotkey),
    /// A file transfer started, moved on or ended.
    Transfer(TransferEvent),
    /// The files chosen in the "Invia file…" dialog (none: cancelled).
    FilesChosen(Vec<PathBuf>),
}

/// Where the window's requests to the host go.
pub struct HostRequests {
    /// Control messages (input, keyframes, the session menu).
    pub control: Outgoing,
    /// File commands (dropped or chosen files, cancellations).
    pub files: UnboundedSender<TransferCommand>,
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
    video: VideoIn,
    link: LinkProbe,
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
    files: WindowFiles,
    window: Option<Window>,
    /// Installed only in full screen (like Remote Desktop): then Alt+Tab,
    /// the Windows key etc. go to the host; in a window they stay local and
    /// keys come from winit's events. Also `None` if it could not be
    /// installed: then system shortcuts stay local even in full screen.
    hook: Option<KeyboardHook>,
    render: Option<RenderThread>,
    stats: Option<ViewerStats>,
    error: Option<String>,
    /// The connection dropped and is being re-established.
    reconnecting: bool,
    /// The host went away on its own (see [`Self::host_left`]).
    host_left: Option<ViewerFailure>,
}

impl ViewerWindow {
    pub fn new(
        options: WindowOptions,
        stream: StreamInfo,
        video: VideoIn,
        link: LinkProbe,
        requests: HostRequests,
        policy: ControlPolicy,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        let HostRequests {
            control: outgoing,
            files,
        } = requests;
        let remote = RemoteControl::new(
            outgoing.clone(),
            policy.enabled(),
            (stream.width, stream.height),
        );
        Self {
            options,
            stream,
            pending: Some(Pending { video, link }),
            outgoing,
            proxy,
            policy,
            remote,
            cursors: CursorCache::default(),
            bar: ControlBar::new(render::toolbar_geometry as _),
            files: WindowFiles::new(files),
            window: None,
            hook: None,
            render: None,
            stats: None,
            error: None,
            reconnecting: false,
            host_left: None,
        }
    }

    pub fn stats(&self) -> Option<&ViewerStats> {
        self.stats.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Set when the host stopped sharing or the connection broke, as
    /// opposed to the user closing the window.
    pub fn host_left(&self) -> Option<&ViewerFailure> {
        self.host_left.as_ref()
    }

    fn open(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let (width, height) = initial_size(self.stream.width, self.stream.height);
        let mut attributes = Window::default_attributes()
            .with_title(&self.options.title)
            .with_inner_size(LogicalSize::new(width, height))
            // Files dropped on the window go to the host. This makes the
            // window's thread an OLE apartment (STA), which is fine: video
            // and network work run elsewhere.
            .with_drag_and_drop(true);
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
        let Pending {
            video: VideoIn { datagrams, restart },
            link,
        } = self.pending.take().context("window opened twice")?;
        let input = RenderInput {
            hwnd,
            stream: self.stream.clone(),
            datagrams,
            outgoing: self.outgoing.clone(),
            link,
            restart,
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
        render.set_reconnecting(self.reconnecting);
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
            Hotkey::ToggleFullscreen => self.toggle_fullscreen(),
            Hotkey::ToggleStats => self.toggle_stats(),
        }
    }

    fn toggle_fullscreen(&mut self) {
        let Some(window) = &self.window else { return };
        let entering = window.fullscreen().is_none();
        self.set_fullscreen(entering);
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
            render.set_toolbar(Some(view));
        }
        self.cursors.set_arrow(window, self.bar.wants_arrow());
    }

    fn run_bar_action(&mut self, event_loop: &ActiveEventLoop, action: BarAction) {
        match action {
            BarAction::ToggleFullscreen => self.toggle_fullscreen(),
            BarAction::ToggleStats => self.toggle_stats(),
            BarAction::Disconnect => self.close(event_loop),
            BarAction::SelectMonitor(index) => self.ask_host(ControlMessage::SelectMonitor(index)),
            BarAction::SetQuality(quality) => self.ask_host(ControlMessage::SetQuality(quality)),
            BarAction::SetAudio(on) => self.ask_host(ControlMessage::SetAudio(on)),
            BarAction::SecureAttention => self.ask_host(ControlMessage::SecureAttention),
            BarAction::SendFiles => self.choose_files(),
            BarAction::CancelTransfers => self.files.cancel_all(),
            BarAction::OpenReceived => {
                if let Some(folder) = self.files.received_folder()
                    && let Err(e) = desktop::open_folder(folder)
                {
                    eprintln!("warning: {e:#}");
                }
            }
        }
    }

    /// Opens the dialog for the files to send. Full screen ends first: the
    /// dialog would open behind it.
    fn choose_files(&mut self) {
        if self
            .window
            .as_ref()
            .is_some_and(|w| w.fullscreen().is_some())
        {
            self.set_fullscreen(false);
        }
        let proxy = self.proxy.clone();
        desktop::choose(Choice::Files, text::FILES_PICK_TITLE, move |chosen| {
            let _ = proxy.send_event(UserEvent::FilesChosen(chosen));
        });
    }

    /// Shows what a transfer event changed: the progress line, the end
    /// message, the "File" menu.
    fn show_transfer(&mut self, event: TransferEvent) {
        let view = self.files.on_event(event);
        if let Some(render) = &self.render {
            render.set_transfers(view.progress);
            if let Some(message) = view.message {
                render.show_message(message);
            }
        }
        self.bar.set_files(view.menu, Instant::now());
        self.sync_bar();
    }

    /// Sends a request the user made to the host.
    fn ask_host(&self, request: ControlMessage) {
        // Fails only once the session is over; nothing left to ask then.
        let _ = self.outgoing.send(request);
    }

    /// The host streams `stream` from now on (another monitor, quality or
    /// resolution): the pointer maps onto its size and the statistics show
    /// it. The window keeps the size the user gave it.
    fn follow_stream(&mut self, stream: StreamInfo) {
        self.remote.set_stream((stream.width, stream.height));
        if let Some(render) = &self.render {
            render.set_stream(stream.clone());
        }
        self.stream = stream;
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

    /// The session broke or came back. Keys held across the break are
    /// forgotten: the host never saw them go down, or no longer sees them.
    fn set_reconnecting(&mut self, reconnecting: bool) {
        self.reconnecting = reconnecting;
        self.remote.reset_keys();
        if let Some(render) = &self.render {
            render.set_reconnecting(reconnecting);
        }
    }

    fn set_host_allows(&mut self, allowed: bool) {
        let notice = self.policy.set_host_allows(allowed);
        let enabled = self.policy.enabled();
        self.remote.set_enabled(enabled);
        if let Some(hook) = &self.hook {
            hook.set_forwarding(enabled);
        }
        if let (Some(notice), Some(render)) = (notice, &self.render) {
            render.show_notice(notice);
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
            WindowEvent::DroppedFile(path) => self.files.dropped(path),
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
            UserEvent::Monitors { monitors, current } => {
                self.bar.set_monitors(monitors, current, Instant::now());
                self.sync_bar();
            }
            UserEvent::StreamChanged(stream) => self.follow_stream(stream),
            UserEvent::Reconnecting => self.set_reconnecting(true),
            UserEvent::Reconnected => self.set_reconnecting(false),
            UserEvent::Hotkey(hotkey) => self.run_hotkey(hotkey),
            UserEvent::Transfer(event) => self.show_transfer(event),
            UserEvent::FilesChosen(chosen) => self.files.send_chosen(chosen),
            UserEvent::HostLeft(failure) => {
                println!("{failure}");
                self.host_left = Some(failure);
                self.close(event_loop);
            }
            UserEvent::RenderFailed(reason) => {
                self.error = Some(reason);
                self.close(event_loop);
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.files.send_dropped();
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
}
