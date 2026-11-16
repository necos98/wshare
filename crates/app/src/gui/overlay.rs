//! The connection indicator: a small always-on-top bar at the top of the
//! screen saying who is connected, with a button to disconnect them.
//!
//! It is a plain Win32 window rather than an egui one: egui draws nothing
//! while its window is hidden in the tray, which is exactly when the
//! indicator matters. It lives on the UI thread, whose message loop
//! (eframe's, or the unattended-access agent's) delivers its messages,
//! and never takes the focus.

use std::cell::{Cell, RefCell};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, DeleteObject,
    DrawTextW, EndPaint, FF_DONTCARE, FW_NORMAL, FW_SEMIBOLD, FillRect, HGDIOBJ, InvalidateRect,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetSystemMetrics,
    GetWindowLongPtrW, HTCLIENT, HWND_TOPMOST, MA_NOACTIVATE, RegisterClassW, SM_CXSCREEN, SW_HIDE,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_ERASEBKGND,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::text;

use super::ports::{AppEvent, Inbox, TrayAction};

const CLASS_NAME: PCWSTR = w!("wshare_connection_indicator");
const DEFAULT_DPI: f32 = 96.0;

// Sizes in logical pixels (at 96 dpi); scaled for the screen.
const WIDTH: i32 = 340;
const HEIGHT: i32 = 40;
const MARGIN_TOP: i32 = 8;
const PADDING: i32 = 12;
const BUTTON_WIDTH: i32 = 100;
const BUTTON_HEIGHT: i32 = 26;
const FONT_POINTS: i32 = 10;
const POINTS_PER_INCH: i32 = 72;

const BACKGROUND: u32 = rgb(32, 34, 37);
const BUTTON_FILL: u32 = rgb(46, 160, 90);
const TEXT_COLOUR: u32 = rgb(255, 255, 255);

/// A GDI `COLORREF` value (0x00BBGGRR).
const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

/// A rectangle in client pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Area {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Area {
    fn contains(&self, x: i32, y: i32) -> bool {
        (self.left..self.right).contains(&x) && (self.top..self.bottom).contains(&y)
    }

    fn rect(&self) -> RECT {
        RECT {
            left: self.left,
            top: self.top,
            right: self.right,
            bottom: self.bottom,
        }
    }
}

/// Where everything goes at a given scale. Pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    width: i32,
    height: i32,
    margin_top: i32,
    padding: i32,
    button: Area,
}

impl Geometry {
    fn at(scale: f32) -> Self {
        let px = |logical: i32| (logical as f32 * scale).round() as i32;
        let (width, height) = (px(WIDTH), px(HEIGHT));
        let (button_width, button_height) = (px(BUTTON_WIDTH), px(BUTTON_HEIGHT));
        let right = width - px(PADDING);
        let top = (height - button_height) / 2;
        Self {
            width,
            height,
            margin_top: px(MARGIN_TOP),
            padding: px(PADDING),
            button: Area {
                left: right - button_width,
                top,
                right,
                bottom: top + button_height,
            },
        }
    }

    /// The area of the message, left of the button.
    fn message(&self) -> Area {
        Area {
            left: self.padding,
            top: 0,
            right: self.button.left - self.padding,
            bottom: self.height,
        }
    }
}

fn system_scale() -> f32 {
    // SAFETY: no preconditions.
    let dpi = unsafe { GetDpiForSystem() };
    dpi as f32 / DEFAULT_DPI
}

/// What the window procedure needs; owned by [`Overlay`] and reached
/// through the window's user data.
struct State {
    inbox: Inbox,
    message: RefCell<Vec<u16>>,
    geometry: Cell<Geometry>,
    scale: Cell<f32>,
}

pub struct Overlay {
    window: HWND,
    state: Box<State>,
    shown: Option<String>,
}

impl Overlay {
    /// Creates the (hidden) window; must run on the UI thread.
    pub fn new(inbox: Inbox) -> Result<Self> {
        let scale = system_scale();
        let state = Box::new(State {
            inbox,
            message: RefCell::new(Vec::new()),
            geometry: Cell::new(Geometry::at(scale)),
            scale: Cell::new(scale),
        });
        // SAFETY: plain Win32 window creation on this thread; the class
        // name and the window procedure live for the whole process.
        let window = unsafe {
            let instance = HINSTANCE(GetModuleHandleW(None).context("module handle")?.0);
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            // Registering twice (a second overlay) fails harmlessly.
            RegisterClassW(&class);
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS_NAME,
                w!("wshare"),
                WS_POPUP,
                0,
                0,
                state.geometry.get().width,
                state.geometry.get().height,
                None,
                None,
                Some(instance),
                None,
            )
            .context("creating the connection indicator")?
        };
        let overlay = Self {
            window,
            state,
            shown: None,
        };
        // SAFETY: the box outlives the window: `Drop` detaches it first.
        unsafe {
            SetWindowLongPtrW(
                window,
                GWLP_USERDATA,
                std::ptr::from_ref::<State>(&overlay.state) as isize,
            );
        }
        Ok(overlay)
    }

    /// Shows the bar with `message`, or hides it for `None`. Cheap to call
    /// on every frame: it acts only when something changed.
    pub fn show(&mut self, message: Option<&str>) {
        if self.shown.as_deref() == message {
            return;
        }
        self.shown = message.map(str::to_string);
        match message {
            Some(message) => self.display(message),
            // SAFETY: `window` is ours and alive.
            None => unsafe {
                let _ = ShowWindow(self.window, SW_HIDE);
            },
        }
    }

    fn display(&self, message: &str) {
        *self.state.message.borrow_mut() = message.encode_utf16().collect();
        let scale = system_scale();
        self.state.scale.set(scale);
        let geometry = Geometry::at(scale);
        self.state.geometry.set(geometry);
        // SAFETY: `window` is ours and alive; the screen metric has no
        // preconditions.
        unsafe {
            let x = (GetSystemMetrics(SM_CXSCREEN) - geometry.width) / 2;
            let _ = SetWindowPos(
                self.window,
                Some(HWND_TOPMOST),
                x,
                geometry.margin_top,
                geometry.width,
                geometry.height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = InvalidateRect(Some(self.window), None, true);
        }
    }

    #[cfg(test)]
    fn window(&self) -> HWND {
        self.window
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // SAFETY: the window is ours; its state is detached before the box
        // is freed, so no message can reach a dangling pointer.
        unsafe {
            SetWindowLongPtrW(self.window, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.window);
        }
    }
}

extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the user data is null or the `State` of the `Overlay` that
    // owns this window, detached before it is freed.
    let state = unsafe { (GetWindowLongPtrW(window, GWLP_USERDATA) as *const State).as_ref() };
    let Some(state) = state else {
        // SAFETY: forwarding the message untouched.
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    };
    match message {
        WM_PAINT => {
            paint(window, state);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // Clicking the bar must not take the focus from what is shared.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTCLIENT as isize),
        WM_LBUTTONUP => {
            let (x, y) = (
                i32::from(lparam.0 as i16),
                i32::from((lparam.0 >> 16) as i16),
            );
            if state.geometry.get().button.contains(x, y) {
                state
                    .inbox
                    .send(AppEvent::Tray(TrayAction::DisconnectViewer));
            }
            LRESULT(0)
        }
        // SAFETY: forwarding the message untouched.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn paint(window: HWND, state: &State) {
    let geometry = state.geometry.get();
    let scale = state.scale.get();
    let mut paint = PAINTSTRUCT::default();
    // SAFETY: GDI calls on the device context of this paint; every object
    // created here is deselected and deleted before `EndPaint`.
    unsafe {
        let dc = BeginPaint(window, &mut paint);
        let whole = Area {
            left: 0,
            top: 0,
            right: geometry.width,
            bottom: geometry.height,
        };
        let background = CreateSolidBrush(COLORREF(BACKGROUND));
        FillRect(dc, &whole.rect(), background);
        let _ = DeleteObject(HGDIOBJ(background.0));

        let button = CreateSolidBrush(COLORREF(BUTTON_FILL));
        FillRect(dc, &geometry.button.rect(), button);
        let _ = DeleteObject(HGDIOBJ(button.0));

        let height =
            -(FONT_POINTS as f32 * DEFAULT_DPI * scale / POINTS_PER_INCH as f32).round() as i32;
        let font = |weight: i32| {
            CreateFontW(
                height,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                FF_DONTCARE.0 as u32,
                w!("Segoe UI"),
            )
        };
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(TEXT_COLOUR));

        let regular = font(FW_NORMAL.0 as i32);
        let previous = SelectObject(dc, HGDIOBJ(regular.0));
        let mut message = state.message.borrow().clone();
        let mut message_area = geometry.message().rect();
        DrawTextW(
            dc,
            &mut message,
            &mut message_area,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );

        let bold = font(FW_SEMIBOLD.0 as i32);
        SelectObject(dc, HGDIOBJ(bold.0));
        let mut label: Vec<u16> = text::DISCONNECT.encode_utf16().collect();
        let mut button_area = geometry.button.rect();
        DrawTextW(
            dc,
            &mut label,
            &mut button_area,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );

        SelectObject(dc, previous);
        let _ = DeleteObject(HGDIOBJ(regular.0));
        let _ = DeleteObject(HGDIOBJ(bold.0));
        let _ = EndPaint(window, &paint);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc::channel;

    use windows::Win32::UI::WindowsAndMessaging::{IsWindowVisible, SendMessageW};

    use super::*;
    use crate::gui::ports::Waker;

    struct NoWaker;

    impl Waker for NoWaker {
        fn wake(&self) {}
    }

    fn overlay() -> (Overlay, std::sync::mpsc::Receiver<AppEvent>) {
        let (tx, rx) = channel();
        let inbox = Inbox::new(tx, Arc::new(NoWaker));
        (Overlay::new(inbox).expect("an overlay"), rx)
    }

    fn click(overlay: &Overlay, x: i32, y: i32) {
        let position = ((y as isize & 0xFFFF) << 16) | (x as isize & 0xFFFF);
        // SAFETY: the window is alive; the message is handled synchronously
        // by our window procedure on this thread.
        unsafe {
            SendMessageW(overlay.window(), WM_LBUTTONUP, None, Some(LPARAM(position)));
        }
    }

    fn visible(overlay: &Overlay) -> bool {
        // SAFETY: the window is alive.
        unsafe { IsWindowVisible(overlay.window()) }.as_bool()
    }

    #[test]
    fn the_button_sits_inside_the_bar_at_any_scale() {
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            let g = Geometry::at(scale);
            assert!(g.button.left > g.padding, "{scale}");
            assert!(g.button.right < g.width, "{scale}");
            assert!(g.button.top >= 0 && g.button.bottom <= g.height, "{scale}");
            assert!(g.message().right > g.message().left, "{scale}");
        }
    }

    #[test]
    fn the_bar_shows_and_hides_with_the_message() {
        let (mut overlay, _events) = overlay();
        assert!(!visible(&overlay));
        overlay.show(Some("PORTATILE è connesso"));
        assert!(visible(&overlay));
        overlay.show(None);
        assert!(!visible(&overlay));
        overlay.show(Some("PORTATILE è connesso"));
        assert!(visible(&overlay));
    }

    #[test]
    fn clicking_the_button_asks_to_disconnect_and_clicking_elsewhere_does_not() {
        let (mut overlay, events) = overlay();
        overlay.show(Some("PORTATILE è connesso"));
        let geometry = overlay.state.geometry.get();

        click(&overlay, geometry.padding, geometry.height / 2);
        assert!(events.try_recv().is_err(), "the message is not a button");

        let button = geometry.button;
        click(
            &overlay,
            (button.left + button.right) / 2,
            (button.top + button.bottom) / 2,
        );
        assert_eq!(
            events.try_recv().unwrap(),
            AppEvent::Tray(TrayAction::DisconnectViewer)
        );
    }
}
