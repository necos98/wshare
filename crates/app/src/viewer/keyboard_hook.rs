//! Low-level keyboard hook, installed only while the viewer is in full
//! screen (like Remote Desktop's default): then every key — including
//! Alt+Tab, the Windows key, Ctrl+Esc — reaches us before Windows acts on
//! it, so it can go to the host instead. In a window, those shortcuts stay
//! with this PC and keys come from the window's own events.
//!
//! The decision logic lives in [`KeyRouter`]; this module is only the
//! Win32 plumbing.
//!
//! Threading matters here. Windows calls a low-level hook on the thread
//! that installed it, and only while that thread is retrieving messages; if
//! it does not answer within a fraction of a second, Windows acts on the key
//! without us (Alt+Tab switches locally) and delivers the call late. The
//! window thread cannot guarantee that (winit sleeps in a wait that does not
//! wake for hook calls), so the hook lives on a dedicated thread that does
//! nothing but `GetMessageW`.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};
use protocol::control::ControlMessage;
use protocol::input::InputEvent;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC, MapVirtualKeyW};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetClassNameW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HC_ACTION,
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP, MSG, PM_NOREMOVE, PeekMessageW,
    PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_QUIT,
};
use super::Outgoing;
use super::key_log;
use super::key_router::{Hotkey, KeyAction, KeyRouter, RawKey};

/// Runs a viewer hotkey; called on the hook thread, so it must be quick
/// (e.g. post an event to the window).
pub type HotkeyHandler = Box<dyn Fn(Hotkey) + Send>;

/// Lives on the hook thread only.
struct HookState {
    forwarding: Arc<AtomicBool>,
    router: KeyRouter,
    outgoing: Outgoing,
    on_hotkey: HotkeyHandler,
}

thread_local! {
    static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}


/// Active while it exists; dropping it uninstalls the hook and ends its
/// thread.
pub struct KeyboardHook {
    thread_id: u32,
    forwarding: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl KeyboardHook {
    /// `forwarding`: remote control on (otherwise only the viewer's own
    /// hotkeys are intercepted).
    pub fn install(
        forwarding: bool,
        outgoing: Outgoing,
        on_hotkey: HotkeyHandler,
    ) -> Result<Self> {
        let forwarding = Arc::new(AtomicBool::new(forwarding));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32>>();
        let state = HookState {
            forwarding: forwarding.clone(),
            router: KeyRouter::default(),
            outgoing,
            on_hotkey,
        };
        let thread = std::thread::Builder::new()
            .name("wshare-keyboard".into())
            .spawn(move || hook_thread(state, ready_tx))
            .context("starting the keyboard hook thread")?;
        let thread_id = ready_rx
            .recv()
            .map_err(|_| anyhow!("keyboard hook thread exited"))??;
        key_log::log(|| {
            format!("hook installed on thread {thread_id}, forwarding={}", forwarding.load(Ordering::Relaxed))
        });
        Ok(Self { thread_id, forwarding, thread: Some(thread) })
    }

    /// Remote control switched on or off mid-session.
    pub fn set_forwarding(&self, forwarding: bool) {
        self.forwarding.store(forwarding, Ordering::Relaxed);
    }
}

impl Drop for KeyboardHook {
    fn drop(&mut self) {
        // Ends GetMessageW on the hook thread, which then unhooks itself.
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Installs the hook, reports readiness, then pumps messages (which is what
/// runs the hook callbacks) until WM_QUIT.
fn hook_thread(state: HookState, ready: mpsc::Sender<Result<u32>>) {
    STATE.with(|slot| *slot.borrow_mut() = Some(state));
    let hook = unsafe {
        // Create this thread's message queue before anyone can post WM_QUIT.
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        GetModuleHandleW(None).and_then(|module| {
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), Some(module.into()), 0)
        })
    };
    let hook = match hook.context("installing the keyboard hook") {
        Ok(hook) => hook,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));

    let mut msg = MSG::default();
    // 0 = WM_QUIT, -1 = error: stop either way.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {}

    unsafe {
        let _ = UnhookWindowsHookEx(hook);
    }
    STATE.with(|slot| slot.borrow_mut().take());
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lparam points to a
        // KBDLLHOOKSTRUCT valid for the duration of the call.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if key_log::enabled() {
            log_call(info);
        }
        let swallow = STATE.with(|state| match state.try_borrow_mut() {
            Ok(mut state) => state.as_mut().is_some_and(|state| handle(state, info)),
            Err(_) => false, // re-entered while handling: let Windows have it
        });
        if swallow {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Diagnostics: the key, how late the hook got it (Windows' event time vs
/// now), and who is in the foreground.
fn log_call(info: &KBDLLHOOKSTRUCT) {
    let late_ms = unsafe { GetTickCount() }.wrapping_sub(info.time);
    let (ours, pid) = viewer_is_foreground();
    let mut class = [0u16; 64];
    let len = unsafe { GetClassNameW(GetForegroundWindow(), &mut class) };
    let class = String::from_utf16_lossy(&class[..len.max(0) as usize]);
    let (vk, scan, flags) = (info.vkCode, info.scanCode, info.flags.0);
    key_log::log(|| {
        format!(
            "hook vk={vk:#04x} scan={scan:#05x} flags={flags:#04x} late={late_ms}ms foreground=pid {pid} ({class}) ours={ours}"
        )
    });
}

/// True when the foreground window belongs to this process — i.e. the
/// viewer window is active. Comparing processes rather than one window
/// handle stays correct whatever windows winit creates internally.
fn viewer_is_foreground() -> (bool, u32) {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid));
    }
    (pid == std::process::id(), pid)
}

/// Returns true when the key must not reach Windows.
fn handle(state: &mut HookState, info: &KBDLLHOOKSTRUCT) -> bool {
    let (ours, _) = viewer_is_foreground();
    // Our own SendInput (or other injectors) must pass untouched.
    if info.flags.0 & LLKHF_INJECTED.0 != 0 {
        return false;
    }
    // Never swallow keys meant for other applications.
    if !ours {
        return false;
    }
    let scan = match info.scanCode {
        0 => unsafe { MapVirtualKeyW(info.vkCode, MAPVK_VK_TO_VSC) },
        scan => scan,
    };
    let key = RawKey {
        scan,
        extended: info.flags.0 & LLKHF_EXTENDED.0 != 0,
        pressed: info.flags.0 & LLKHF_UP.0 == 0,
    };
    let forwarding = state.forwarding.load(Ordering::Relaxed);
    let action = state.router.route(key, forwarding);
    key_log::log(|| format!("    -> {action:?} (forwarding={forwarding})"));
    match action {
        KeyAction::Hotkey(hotkey) => {
            if forwarding {
                // Ctrl and Alt already went to the host: release them there.
                let _ = state.outgoing.send(ControlMessage::Input(InputEvent::ReleaseAll));
            }
            (state.on_hotkey)(hotkey);
            true
        }
        KeyAction::Forward(event) => {
            let _ = state.outgoing.send(ControlMessage::Input(event));
            true
        }
        KeyAction::Swallow => true,
        KeyAction::PassThrough => false,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    /// Installs and removes the hook without pressing any key: checks the
    /// dedicated thread starts, answers WM_QUIT and is joined promptly.
    /// (While installed, keys pass through: this process is never in the
    /// foreground.)
    #[test]
    fn hook_thread_installs_and_stops_promptly() {
        let (tx, _rx) = unbounded_channel();
        let hook = KeyboardHook::install(true, tx, Box::new(|_| {})).expect("hook installs");
        hook.set_forwarding(false);
        assert!(!hook.forwarding.load(Ordering::Relaxed));
        let started = Instant::now();
        drop(hook);
        assert!(started.elapsed() < Duration::from_secs(1), "hook thread did not stop");
    }

    #[test]
    fn hook_can_be_reinstalled_after_removal() {
        for _ in 0..3 {
            let (tx, _rx) = unbounded_channel();
            drop(KeyboardHook::install(true, tx, Box::new(|_| {})).expect("hook installs"));
        }
    }
}
