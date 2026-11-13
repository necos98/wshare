//! The viewer window (winit). It only manages the window's lifetime and
//! forwards resizes; all video work happens on the render thread.

use std::sync::mpsc::Receiver;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use pipeline::ViewerStats;
use protocol::control::StreamInfo;
use protocol::input::CursorShape;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId};

use super::Outgoing;
use super::cursor::CursorCache;
use super::remote::RemoteControl;
use super::render_thread::{RenderInput, RenderThread};

/// Largest initial window, in logical pixels; the user can resize freely.
const MAX_INITIAL: (f64, f64) = (1280.0, 800.0);

#[derive(Debug)]
pub enum UserEvent {
    HostLeft(String),
    RenderFailed(String),
    Cursor(CursorShape),
    CursorVisible(bool),
}

pub struct ViewerWindow {
    title: String,
    stream: StreamInfo,
    /// Taken when the render thread starts.
    pending: Option<(Receiver<Bytes>, Outgoing)>,
    proxy: EventLoopProxy<UserEvent>,
    remote: RemoteControl,
    cursors: CursorCache,
    window: Option<Window>,
    render: Option<RenderThread>,
    stats: Option<ViewerStats>,
    error: Option<String>,
}

impl ViewerWindow {
    pub fn new(
        title: String,
        stream: StreamInfo,
        datagrams: Receiver<Bytes>,
        outgoing: Outgoing,
        input_allowed: bool,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        let remote =
            RemoteControl::new(outgoing.clone(), input_allowed, (stream.width, stream.height));
        Self {
            title,
            stream,
            pending: Some((datagrams, outgoing)),
            proxy,
            remote,
            cursors: CursorCache::default(),
            window: None,
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
        let attributes = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(LogicalSize::new(width, height))
            // OLE drag-and-drop would force an STA on this thread; unneeded.
            .with_drag_and_drop(false);
        let window = event_loop.create_window(attributes)?;
        let RawWindowHandle::Win32(handle) = window.window_handle()?.as_raw() else {
            bail!("not a Win32 window");
        };
        let PhysicalSize { width, height } = window.inner_size();
        let (datagrams, outgoing) = self.pending.take().context("window opened twice")?;
        let proxy = self.proxy.clone();
        let input = RenderInput { hwnd: handle.hwnd.get(), stream: self.stream.clone(), datagrams, outgoing };
        self.render = Some(RenderThread::start(input, width, height, move |result| {
            if let Err(e) = result {
                let _ = proxy.send_event(UserEvent::RenderFailed(format!("{e:#}")));
            }
        }));
        self.window = Some(window);
        Ok(())
    }

    /// Joins the render thread (it draws into the window) before closing.
    fn close(&mut self, event_loop: &ActiveEventLoop) {
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
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(window) = &self.window {
                    self.remote.pointer_moved(position, window.inner_size());
                }
            }
            WindowEvent::MouseInput { state, button, .. } => self.remote.button(button, state),
            WindowEvent::MouseWheel { delta, .. } => self.remote.wheel(delta),
            // Synthetic events are winit replaying keys held while focus
            // changed; ReleaseAll on focus loss already covers them.
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => {
                self.remote.key(event.physical_key, event.state);
            }
            WindowEvent::Focused(false) => self.remote.focus_lost(),
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Cursor(shape) => {
                if let Some(window) = &self.window {
                    self.cursors.apply(event_loop, window, shape);
                }
                return;
            }
            UserEvent::CursorVisible(visible) => {
                if let Some(window) = &self.window {
                    window.set_cursor_visible(visible);
                }
                return;
            }
            UserEvent::HostLeft(reason) => println!("{reason}"),
            UserEvent::RenderFailed(reason) => self.error = Some(reason),
        }
        self.close(event_loop);
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
