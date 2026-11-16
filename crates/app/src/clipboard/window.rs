//! The window that listens for clipboard changes: a message-only window
//! (never shown, no taskbar button) on a thread of its own that does
//! nothing but pump messages, registered with `AddClipboardFormatListener`.
//! It is also the owner the clipboard is opened with when writing: opening
//! it without an owner makes `SetClipboardData` fail.

use std::cell::RefCell;
use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, RemoveClipboardFormatListener,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_MESSAGE,
    MSG, PostMessageW, PostQuitMessage, RegisterClassW, UnregisterClassW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CLIPBOARDUPDATE, WM_CLOSE, WM_DESTROY, WNDCLASSW,
};
use windows::core::{PCWSTR, w};

const CLASS_NAME: PCWSTR = w!("wshare_clipboard_listener");

/// The window clipboard operations are opened with. A plain number so it
/// can cross threads; only valid while its [`ClipboardWindow`] lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardOwner(usize);

impl ClipboardOwner {
    pub fn handle(self) -> HWND {
        HWND(self.0 as *mut _)
    }
}

thread_local! {
    /// The window procedure's way to reach the receiver of the changes: it
    /// runs on the listener thread only.
    static CHANGES: RefCell<Option<UnboundedSender<()>>> = const { RefCell::new(None) };
}

/// Open while it exists; dropping it closes the window and ends its thread.
pub struct ClipboardWindow {
    owner: ClipboardOwner,
    thread: Option<JoinHandle<()>>,
}

impl ClipboardWindow {
    /// Opens the window; every change of the clipboard (by any program,
    /// this one included) sends one `()` on the returned receiver.
    pub fn open() -> Result<(Self, UnboundedReceiver<()>)> {
        let (changes_tx, changes_rx) = unbounded_channel();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<usize>>();
        let thread = std::thread::Builder::new()
            .name("wshare-clipboard".into())
            .spawn(move || listener_thread(changes_tx, ready_tx))
            .context("starting the clipboard listener thread")?;
        let handle = ready_rx
            .recv()
            .map_err(|_| anyhow!("the clipboard listener thread ended at once"))??;
        let window = Self {
            owner: ClipboardOwner(handle),
            thread: Some(thread),
        };
        Ok((window, changes_rx))
    }

    pub fn owner(&self) -> ClipboardOwner {
        self.owner
    }
}

impl Drop for ClipboardWindow {
    fn drop(&mut self) {
        // WM_CLOSE destroys the window, whose WM_DESTROY ends the thread's
        // message loop.
        // SAFETY: posting a message to a window handle is sound for any
        // handle value; a stale one just fails.
        let _ = unsafe { PostMessageW(Some(self.owner.handle()), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Creates the window, reports its handle (or why not), then pumps
/// messages until the window is destroyed.
fn listener_thread(changes: UnboundedSender<()>, ready: mpsc::Sender<Result<usize>>) {
    CHANGES.with(|slot| *slot.borrow_mut() = Some(changes));
    match create_window() {
        Ok(window) => {
            let _ = ready.send(Ok(window.0 as usize));
            pump_messages();
            // SAFETY: the class was registered by `create_window` on this
            // thread, and its only window is gone.
            let _ = unsafe { UnregisterClassW(CLASS_NAME, module()) };
        }
        Err(e) => {
            let _ = ready.send(Err(e));
        }
    }
    CHANGES.with(|slot| *slot.borrow_mut() = None);
}

fn module() -> Option<windows::Win32::Foundation::HINSTANCE> {
    // SAFETY: querying the handle of this executable has no preconditions.
    unsafe { GetModuleHandleW(None) }.ok().map(Into::into)
}

fn create_window() -> Result<HWND> {
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: module().unwrap_or_default(),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialised and outlives the call.
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(anyhow!(
            "registering the clipboard window class: {}",
            windows::core::Error::from_thread()
        ));
    }
    // SAFETY: the class is registered; a message-only window has no
    // parent besides `HWND_MESSAGE` and needs no menu or extra data.
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!("wshare clipboard"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            module(),
            None,
        )
    }
    .context("creating the clipboard window")?;
    // SAFETY: `window` is the live window of this thread.
    unsafe { AddClipboardFormatListener(window) }
        .context("listening for clipboard changes")
        .inspect_err(|_| {
            // SAFETY: as above; undoes the creation.
            let _ = unsafe { DestroyWindow(window) };
        })?;
    Ok(window)
}

fn pump_messages() {
    let mut message = MSG::default();
    // 0 = WM_QUIT, -1 = error: stop either way.
    // SAFETY: `message` is a valid, exclusively borrowed MSG.
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        // SAFETY: `message` was just filled in by `GetMessageW`.
        unsafe { DispatchMessageW(&message) };
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLIPBOARDUPDATE => {
            CHANGES.with(|slot| {
                if let Some(changes) = slot.borrow().as_ref() {
                    // Fails only once the session is over.
                    let _ = changes.send(());
                }
            });
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: both calls are made on the thread that owns `window`.
            unsafe {
                let _ = RemoveClipboardFormatListener(window);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        // SAFETY: the arguments are the ones Windows gave this procedure.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}
