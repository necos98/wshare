//! The window that listens for clipboard changes: a message-only window
//! (never shown, no taskbar button) on a thread of its own that does
//! nothing but pump messages, registered with `AddClipboardFormatListener`.
//! It is also the owner the clipboard is opened with when writing: opening
//! it without an owner makes `SetClipboardData` fail.
//!
//! The thread is also the OLE apartment (STA) of the other PC's files put
//! on the clipboard (see `virtual_files`): OLE needs a thread that pumps
//! messages, and Explorer's calls into those files (a paste reading from
//! the network) then wait here, never on a window's thread.

use std::cell::RefCell;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, GetClipboardOwner, RemoveClipboardFormatListener,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleInitialize, OleSetClipboard, OleUninitialize};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowThreadProcessId, HWND_MESSAGE, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    UnregisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLIPBOARDUPDATE, WM_CLOSE,
    WM_DESTROY, WNDCLASSW,
};
use windows::core::{PCWSTR, w};

use super::access::AccessError;
use super::virtual_files::{RemoteDataObject, RemoteFiles};

const CLASS_NAME: PCWSTR = w!("wshare_clipboard_listener");

/// Posted when files wait in [`FILE_OFFERS`] to be put on the clipboard.
const WM_OFFER_FILES: u32 = WM_APP + 1;

/// Posted to take the files offered last off the clipboard.
const WM_WITHDRAW_FILES: u32 = WM_APP + 2;

/// How long putting files on the clipboard may take.
const OFFER_TIMEOUT: Duration = Duration::from_secs(5);

/// The window clipboard operations are opened with. A plain number so it
/// can cross threads; only valid while its [`ClipboardWindow`] lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardOwner(usize);

impl ClipboardOwner {
    pub fn handle(self) -> HWND {
        HWND(self.0 as *mut _)
    }
}

/// Files to put on the clipboard, and where to say how it went.
struct FileOffer {
    files: RemoteFiles,
    done: mpsc::Sender<Result<(), String>>,
}

/// Puts files on the clipboard from any thread, through the window's.
#[derive(Clone)]
pub struct FileOffers {
    owner: ClipboardOwner,
    offers: mpsc::Sender<FileOffer>,
}

impl FileOffers {
    pub fn offer(&self, files: RemoteFiles) -> Result<(), AccessError> {
        let (done, outcome) = mpsc::channel();
        self.offers
            .send(FileOffer { files, done })
            .map_err(|_| AccessError::Failed("the clipboard listener is gone".into()))?;
        // SAFETY: posting to a window handle is sound for any value; a
        // stale one just fails.
        unsafe {
            PostMessageW(
                Some(self.owner.handle()),
                WM_OFFER_FILES,
                WPARAM(0),
                LPARAM(0),
            )
        }
        .map_err(|e| AccessError::Failed(format!("reaching the clipboard thread: {e}")))?;
        outcome
            .recv_timeout(OFFER_TIMEOUT)
            .map_err(|_| AccessError::Failed("the clipboard thread did not answer".into()))?
            .map_err(AccessError::Failed)
    }

    /// Takes the files offered last off the clipboard, if it still holds
    /// them (anything copied since stays). Does not wait for it.
    pub fn withdraw(&self) -> Result<(), AccessError> {
        // SAFETY: as in `offer`.
        unsafe {
            PostMessageW(
                Some(self.owner.handle()),
                WM_WITHDRAW_FILES,
                WPARAM(0),
                LPARAM(0),
            )
        }
        .map_err(|e| AccessError::Failed(format!("reaching the clipboard thread: {e}")))
    }
}

thread_local! {
    /// The window procedure's way to reach the receiver of the changes: it
    /// runs on the listener thread only.
    static CHANGES: RefCell<Option<UnboundedSender<()>>> = const { RefCell::new(None) };
    /// Files waiting to be put on the clipboard, and the OLE apartment's
    /// state: `None` if OLE could not start on this thread.
    static FILE_OFFERS: RefCell<Option<mpsc::Receiver<FileOffer>>> = const { RefCell::new(None) };
    /// The files this thread put on the clipboard last.
    static OFFERED: RefCell<Option<IDataObject>> = const { RefCell::new(None) };
}

/// Open while it exists; dropping it closes the window and ends its thread.
pub struct ClipboardWindow {
    owner: ClipboardOwner,
    offers: mpsc::Sender<FileOffer>,
    thread: Option<JoinHandle<()>>,
}

impl ClipboardWindow {
    /// Opens the window; every change of the clipboard (by any program,
    /// this one included) sends one `()` on the returned receiver.
    pub fn open() -> Result<(Self, UnboundedReceiver<()>)> {
        let (changes_tx, changes_rx) = unbounded_channel();
        let (offers_tx, offers_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<usize>>();
        let thread = std::thread::Builder::new()
            .name("wshare-clipboard".into())
            .spawn(move || listener_thread(changes_tx, offers_rx, ready_tx))
            .context("starting the clipboard listener thread")?;
        let handle = ready_rx
            .recv()
            .map_err(|_| anyhow!("the clipboard listener thread ended at once"))??;
        let window = Self {
            owner: ClipboardOwner(handle),
            offers: offers_tx,
            thread: Some(thread),
        };
        Ok((window, changes_rx))
    }

    pub fn owner(&self) -> ClipboardOwner {
        self.owner
    }

    pub fn file_offers(&self) -> FileOffers {
        FileOffers {
            owner: self.owner,
            offers: self.offers.clone(),
        }
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
fn listener_thread(
    changes: UnboundedSender<()>,
    offers: mpsc::Receiver<FileOffer>,
    ready: mpsc::Sender<Result<usize>>,
) {
    CHANGES.with(|slot| *slot.borrow_mut() = Some(changes));
    // Without OLE the text is still shared; only files cannot be offered.
    // SAFETY: called once, on this thread, before any OLE use.
    let ole = unsafe { OleInitialize(None) }.is_ok();
    if ole {
        FILE_OFFERS.with(|slot| *slot.borrow_mut() = Some(offers));
    }
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
    FILE_OFFERS.with(|slot| *slot.borrow_mut() = None);
    if ole {
        // SAFETY: pairs the successful `OleInitialize` above.
        unsafe { OleUninitialize() };
    }
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

/// Puts the waiting files on the clipboard, answering each request.
fn offer_waiting_files() {
    let waiting: Vec<FileOffer> = FILE_OFFERS.with(|slot| match slot.borrow().as_ref() {
        Some(offers) => offers.try_iter().collect(),
        None => Vec::new(),
    });
    for FileOffer { files, done } in waiting {
        let object = RemoteDataObject::create(files);
        // SAFETY: OLE is initialised on this thread (offers only reach it
        // then), which pumps messages as the OLE clipboard requires.
        let outcome = unsafe { OleSetClipboard(&object) }
            .map(|()| OFFERED.with(|slot| *slot.borrow_mut() = Some(object)))
            .map_err(|e| format!("putting the files on the clipboard: {e}"));
        let _ = done.send(outcome);
    }
}

/// Takes our files off the clipboard if they are still what it holds: they
/// cannot be pasted once the session is over, or no longer allowed.
/// Anything copied since stays.
fn withdraw_offered_files() {
    if OFFERED.with(|slot| slot.borrow_mut().take()).is_none() {
        return;
    }
    // SAFETY: plain queries; OLE's clipboard window belongs to this thread
    // while our files are on the clipboard.
    let ours = unsafe {
        let owner = GetClipboardOwner().unwrap_or_default();
        !owner.is_invalid() && GetWindowThreadProcessId(owner, None) == GetCurrentThreadId()
    };
    if ours {
        // SAFETY: OLE is initialised on this thread.
        let _ = unsafe { OleSetClipboard(None) };
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
        WM_OFFER_FILES => {
            offer_waiting_files();
            LRESULT(0)
        }
        WM_WITHDRAW_FILES => {
            withdraw_offered_files();
            LRESULT(0)
        }
        WM_DESTROY => {
            withdraw_offered_files();
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
