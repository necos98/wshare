//! `wshare service agent`: the host of unattended access, started by the
//! service as SYSTEM in the session at the console.
//!
//! - It shares with the service's identity and authorized devices (its
//!   data folder is the protected one) and admits only authorized devices,
//!   with the password if one is set.
//! - Its capture and input threads follow the input desktop, so the viewer
//!   sees and uses the lock screen, the sign-in screen and UAC prompts.
//! - Ctrl+Alt+Del from the viewer is passed to the service (on standard
//!   output), the only one allowed to generate it.
//! - While a viewer is connected, the connection indicator stays at the
//!   top of the screen, with its button to disconnect them.
//! - It stops when the service closes its standard input; what happens is
//!   logged on standard error (the service log).

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
    TranslateMessage, WM_APP, WM_QUIT, WM_USER,
};

use super::command::AgentRequest;
use super::config::ServiceConfig;
use super::{UnattendedPolicy, locations};
use crate::access_mode::AccessMode;
use crate::gui::overlay::Overlay;
use crate::gui::ports::{AppEvent, Inbox, TrayAction, Waker};
use crate::host::{HostEvent, HostHandle, HostObserver};
use crate::options::HostOptions;
use crate::paths;
use crate::text;
use crate::trust::Timestamp;

/// Posted to the main thread when there are events to read.
const WM_WAKE: u32 = WM_APP + 1;

pub(super) fn run() -> Result<()> {
    let data = locations::data_dir()?;
    paths::use_config_dir(data.clone())?;
    let config = ServiceConfig::load(&locations::config_file(&data))?;
    input_desktop::follow_input_desktop();

    // Messages posted to a thread without a queue are lost: the host may
    // wake this one before its loop runs.
    create_message_queue();
    let waker = Arc::new(ThreadWaker::current());
    let (events, host_events) = channel();
    let observer = Observer {
        events,
        waker: Arc::clone(&waker),
    };
    let host = HostHandle::start(host_options(config), observer)?;
    stop_when_the_service_says(waker.thread)?;
    let (inbox, clicks) = channel();
    // Without the indicator the session still works: the log says why.
    let mut indicator = Overlay::new(Inbox::new(inbox, waker))
        .inspect_err(|e| log(format_args!("no connection indicator: {e:#}")))
        .ok();
    let result = message_loop(&host, &host_events, &clicks, indicator.as_mut());
    host.stop();
    result
}

/// Unattended access shares the screen, the clipboard and the sound as the
/// host's settings say (copied by `install`), always with remote control.
/// Files are not received: they would land as SYSTEM in the profile of
/// nobody. The access mode does not count (see `HostOptions::unattended`).
fn host_options(config: ServiceConfig) -> HostOptions {
    let settings = config.settings;
    HostOptions {
        stream: settings.stream,
        port: settings.port,
        pin: None,
        view_only: false,
        share_clipboard: settings.share_clipboard,
        access: AccessMode::PinOnly,
        receive_files: false,
        share_audio: settings.share_audio,
        downloads: None,
        unattended: Some(UnattendedPolicy {
            password: config.password,
        }),
    }
}

/// Gives this thread its message queue (Windows creates it on the first
/// call that reads one).
fn create_message_queue() {
    let mut message = MSG::default();
    // SAFETY: a valid out-parameter; nothing is removed from the queue.
    let _ = unsafe { PeekMessageW(&mut message, None, WM_USER, WM_USER, PM_NOREMOVE) };
}

/// Wakes the main thread's message loop.
struct ThreadWaker {
    thread: u32,
}

impl ThreadWaker {
    fn current() -> Self {
        // SAFETY: no preconditions.
        Self {
            thread: unsafe { GetCurrentThreadId() },
        }
    }
}

impl Waker for ThreadWaker {
    fn wake(&self) {
        post(self.thread, WM_WAKE);
    }
}

fn post(thread: u32, message: u32) {
    // SAFETY: plain Win32 call; a thread that is gone just drops it.
    let _ = unsafe { PostThreadMessageW(thread, message, WPARAM(0), LPARAM(0)) };
}

/// Passes the host's events to the main thread; a Ctrl+Alt+Del request
/// goes straight to the service.
struct Observer {
    events: Sender<HostEvent>,
    waker: Arc<ThreadWaker>,
}

impl HostObserver for Observer {
    fn on_event(&self, event: HostEvent) {
        if matches!(event, HostEvent::SecureAttentionRequested) {
            ask_service(AgentRequest::SecureAttention);
            return;
        }
        if self.events.send(event).is_ok() {
            self.waker.wake();
        }
    }
}

fn ask_service(request: AgentRequest) {
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{}", request.to_line())
        .and_then(|()| out.flush())
        .is_err()
    {
        log("the service is not listening: Ctrl+Alt+Del not sent");
    }
}

/// Ends the message loop once the service closes our standard input.
fn stop_when_the_service_says(thread: u32) -> Result<()> {
    std::thread::Builder::new()
        .name("wshare-agent-stdin".into())
        .spawn(move || {
            let mut sink = [0u8; 64];
            let mut stdin = std::io::stdin().lock();
            while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
            post(thread, WM_QUIT);
        })
        .context("watching the service")?;
    Ok(())
}

/// Runs the indicator's window and handles events until the service asks
/// to stop (`Ok`) or the host stops on its own (`Err`).
fn message_loop(
    host: &HostHandle,
    events: &Receiver<HostEvent>,
    clicks: &Receiver<AppEvent>,
    mut indicator: Option<&mut Overlay>,
) -> Result<()> {
    let mut message = MSG::default();
    loop {
        // SAFETY: `message` is a valid out-parameter; messages of any
        // window of this thread (the indicator) are dispatched below.
        match unsafe { GetMessageW(&mut message, None, 0, 0) }.0 {
            0 => return Ok(()),
            -1 => bail!("the message loop failed"),
            _ => {}
        }
        if message.hwnd.is_invalid() && message.message == WM_WAKE {
            for event in events.try_iter() {
                on_host_event(event, indicator.as_deref_mut())?;
            }
            for click in clicks.try_iter() {
                if click == AppEvent::Tray(TrayAction::DisconnectViewer) {
                    host.disconnect_viewer();
                }
            }
            continue;
        }
        // SAFETY: a message just received on this thread.
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// Logs what matters and keeps the indicator in step; the end of the host
/// ends the agent (the service starts another).
fn on_host_event(event: HostEvent, indicator: Option<&mut Overlay>) -> Result<()> {
    let show = |message: Option<&str>| {
        if let Some(indicator) = indicator {
            indicator.show(message);
        }
    };
    match event {
        HostEvent::Listening {
            port, fingerprint, ..
        } => log(format_args!(
            "listening on UDP {port}, identity {fingerprint}"
        )),
        HostEvent::ViewerConnected {
            viewer_name,
            address,
        } => {
            log(format_args!("{viewer_name} ({address}) connected"));
            show(Some(&text::unattended_indicator(&viewer_name)));
        }
        HostEvent::ViewerRejected { address, reason } => {
            log(format_args!("{address} refused: {reason}"));
        }
        HostEvent::ViewerDisconnected { address, error, .. } => {
            match error {
                Some(error) => log(format_args!("{address} disconnected: {error}")),
                None => log(format_args!("{address} disconnected")),
            }
            show(None);
        }
        HostEvent::Warning(warning) => log(format_args!("warning: {warning}")),
        HostEvent::Stopped { error: Some(error) } => bail!("sharing stopped: {error}"),
        HostEvent::Stopped { error: None } => log("sharing stopped"),
        _ => {}
    }
    Ok(())
}

/// One dated line of the service log.
fn log(line: impl std::fmt::Display) {
    eprintln!("{} agent: {line}", Timestamp::now());
}

#[cfg(test)]
mod tests {
    use codec::Codec;

    use super::*;
    use crate::options::StreamOptions;
    use crate::unattended::service_settings::ServiceSettings;

    fn loaded(text: &str) -> ServiceConfig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.toml");
        std::fs::write(&path, text).unwrap();
        ServiceConfig::load(&path).unwrap()
    }

    #[test]
    fn the_host_follows_the_settings_copied_to_the_service() {
        let config = ServiceConfig {
            settings: ServiceSettings {
                port: 9000,
                stream: StreamOptions {
                    monitor: 1,
                    fps: 30,
                    mbps: 8,
                    codec: Codec::Hevc,
                },
                share_clipboard: false,
                share_audio: false,
            },
            password: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.toml");
        config.save(&path).unwrap();

        let options = host_options(ServiceConfig::load(&path).unwrap());
        assert!(!options.share_clipboard, "the clipboard stays private");
        assert!(!options.share_audio);
        assert_eq!(options.port, 9000);
        assert_eq!(options.stream, config.settings.stream);
    }

    #[test]
    fn a_service_toml_without_the_new_settings_keeps_what_it_did() {
        let options = host_options(loaded("port = 7420\n"));
        assert!(options.share_clipboard && options.share_audio);
        assert_eq!(options.stream, StreamOptions::default());
        assert_eq!(options.port, 7420);
    }

    #[test]
    fn only_what_makes_sense_unattended_is_fixed() {
        let options = host_options(loaded("port = 7420\nshare_clipboard = false\n"));
        assert!(!options.share_clipboard);
        assert!(!options.view_only, "remote control is the point");
        assert!(!options.receive_files, "no files as SYSTEM");
        assert_eq!(options.pin, None);
        assert!(options.unattended.is_some());
    }
}
