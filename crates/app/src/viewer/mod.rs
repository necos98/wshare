//! `wshare view`: connect to a host and show its screen.
//!
//! The viewer presents its own certificate, so a host that admitted it once
//! with the PIN lets it in without one from then on; such a host is
//! remembered and pinned by fingerprint (see `pairing`). Without a PIN
//! given, a remembered host is tried without one first and the PIN is asked
//! only if the host requires it again.
//!
//! Threads:
//! - main thread: the window (winit requires it);
//! - tokio runtime: control channel and datagram reception;
//! - render thread: owns the GPU objects, runs `ViewerStreamer`
//!   (reassemble -> decode -> present).

mod cursor;
pub mod failure;
mod input_map;
mod key_log;
mod key_router;
mod keyboard_hook;
mod pairing;
mod remote;
mod render_thread;
pub mod report;
mod sink;
mod stats;
mod toolbar;
mod window;

use std::io::Write;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use bytes::Bytes;
use discovery::computer_name;
use net::{ControlReceiver, ControlSender, Fingerprint, SessionHandle, VideoChannel};
use protocol::control::{ControlMessage, PROTOCOL_VERSION, RejectReason, StreamInfo};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use winit::event_loop::{DeviceEvents, EventLoop, EventLoopProxy};

use crate::cli::ViewOptions;
use crate::options::DEFAULT_PORT;
use crate::target::{self, Resolved, parse_target};
use failure::ViewerFailure;
use pairing::Pairing;
use report::{REPORT_ENV, ViewerReport};
use window::{ControlPolicy, UserEvent, ViewerWindow, WindowOptions};

/// Environment variable holding the PIN when `--pin` is absent (used by
/// the desktop interface, so the PIN is not visible on the command line).
pub const PIN_ENV: &str = "WSHARE_PIN";

const DISCOVERY_WAIT: Duration = Duration::from_secs(3);
const WELCOME_TIMEOUT: Duration = Duration::from_secs(10);
/// Time given to the Bye message to leave before the connection closes.
const GOODBYE_GRACE: Duration = Duration::from_millis(300);

/// A session that passed the handshake.
struct Connected {
    host_name: String,
    stream: StreamInfo,
    input_allowed: bool,
    fingerprint: Fingerprint,
    tx: ControlSender,
    rx: ControlReceiver,
    video: VideoChannel,
    handle: SessionHandle,
}

pub fn run(options: ViewOptions) -> Result<()> {
    let target = parse_target(options.target.as_deref(), DEFAULT_PORT);
    if !matches!(target, target::Target::Address(_)) {
        println!("Searching the LAN for hosts...");
    }
    let Resolved {
        addr,
        label,
        host_id,
    } = target::resolve(&target, DISCOVERY_WAIT)?;
    let mut pairing = Pairing::load(options.host_id.as_deref().or(host_id.as_deref()));
    let given = given_pin(options.pin, std::env::var(PIN_ENV).ok());
    let reporting = report::reporting_enabled(std::env::var(REPORT_ENV).ok().as_deref());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let connected = authenticate(&runtime, addr, &label, &pairing, given)?;
    let Connected {
        host_name,
        stream,
        input_allowed,
        fingerprint,
        tx,
        rx,
        video,
        handle,
    } = connected;
    // Before the "Connected" line: the desktop interface re-reads the known
    // hosts when it sees that line.
    pairing.remember(fingerprint, &host_name);
    if reporting {
        println!(
            "{}",
            ViewerReport::Connected {
                host_name: host_name.clone(),
            }
            .to_line()
        );
    }
    println!(
        "Connected to \"{host_name}\" ({addr}) - {}x{} @ {} fps {:?}",
        stream.width, stream.height, stream.fps, stream.codec
    );
    println!("Host fingerprint: {fingerprint}");
    let policy = ControlPolicy {
        host_allows: input_allowed,
        view_only: options.view_only,
    };
    if !policy.enabled() {
        println!("View only: mouse and keyboard stay local.");
    }
    println!("Ctrl+Alt+Enter: full screen - Ctrl+Alt+I: statistics");

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    // No raw input: by default winit registers keyboard and mouse as raw
    // input devices, and while a process with that registration is in the
    // foreground Windows skips its own low-level keyboard hook — full
    // screen would then leave Alt+Tab and the Windows key local. The viewer
    // uses only window events, never `DeviceEvent`s.
    event_loop.listen_device_events(DeviceEvents::Never);
    let (datagram_tx, datagram_rx) = std::sync::mpsc::channel::<Bytes>();
    let (outgoing_tx, outgoing_rx) = unbounded_channel::<ControlMessage>();
    runtime.spawn(receive_video(video, datagram_tx));
    let control = runtime.spawn(run_control(tx, rx, outgoing_rx, event_loop.create_proxy()));

    let rtt_handle = handle.clone();
    let mut window = ViewerWindow::new(
        WindowOptions {
            title: format!("{host_name} - wshare"),
            fullscreen: options.fullscreen,
            stats: options.stats,
        },
        stream,
        datagram_rx,
        Box::new(move || rtt_handle.rtt()),
        outgoing_tx.clone(),
        policy,
        event_loop.create_proxy(),
    );
    event_loop.run_app(&mut window)?;

    let _ = outgoing_tx.send(ControlMessage::Bye);
    runtime.block_on(async {
        let _ = tokio::time::timeout(GOODBYE_GRACE, control).await;
        handle.close("viewer closed");
    });
    if let Some(stats) = window.stats() {
        println!(
            "Session: {} frames shown, {} lost, {} skipped, {} keyframes requested",
            stats.frames_presented,
            stats.frames_lost,
            stats.frames_skipped,
            stats.keyframes_requested
        );
    }
    if let Some(e) = window.error() {
        bail!("{e}");
    }
    // A terminal user closes the window or sees the message; the interface
    // needs to know why the viewer ended.
    match window.host_left() {
        Some(failure) if reporting => Err(failure.clone().into()),
        _ => Ok(()),
    }
}

/// The exit code of `wshare view` that ended with `error`. When the desktop
/// interface asks for it, the cause is also written as a status line.
pub fn exit_code_for(error: &anyhow::Error) -> i32 {
    let cause = failure::cause_of(error);
    if report::reporting_enabled(std::env::var(REPORT_ENV).ok().as_deref()) {
        println!("{}", ViewerReport::Failed(cause.clone()).to_line());
    }
    cause.exit_code()
}

/// What the first connection attempt sends.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FirstAttempt {
    Pin(String),
    /// A remembered host: it should let us in without one.
    WithoutPin,
    AskPin,
}

fn first_attempt(given: Option<String>, host_remembered: bool) -> FirstAttempt {
    match (given, host_remembered) {
        (Some(pin), _) => FirstAttempt::Pin(pin),
        (None, true) => FirstAttempt::WithoutPin,
        (None, false) => FirstAttempt::AskPin,
    }
}

/// Connects, asking for the PIN when the host requires one we did not
/// send. Gives up (with the host's answer) when no PIN is typed, e.g. when
/// started by the desktop interface, which has no terminal.
fn authenticate(
    runtime: &Runtime,
    addr: SocketAddr,
    label: &str,
    pairing: &Pairing,
    given: Option<String>,
) -> Result<Connected> {
    let mut pin = match first_attempt(given, pairing.expected().is_some()) {
        FirstAttempt::Pin(pin) => Some(pin),
        FirstAttempt::WithoutPin => None,
        FirstAttempt::AskPin => ask_pin(&format!("PIN shown on {label}: "))?,
    };
    loop {
        match runtime.block_on(connect(addr, pairing, pin.clone())) {
            Err(e) if pin.is_none() && is_pin_required(&e) => {
                pin = ask_pin(&format!("{label} requires the PIN: "))?;
                if pin.is_none() {
                    return Err(e);
                }
            }
            result => return result,
        }
    }
}

fn is_pin_required(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ViewerFailure>()
        == Some(&ViewerFailure::Rejected(RejectReason::PinRequired))
}

/// One connection attempt: TLS (pinned for a remembered host, presenting
/// our certificate), then Hello and the host's answer.
async fn connect(addr: SocketAddr, pairing: &Pairing, pin: Option<String>) -> Result<Connected> {
    let session = net::connect(addr, pairing.expected(), pairing.identity())
        .await
        .map_err(failure::connect_failure)?;
    let fingerprint = session
        .peer_fingerprint()
        .ok_or_else(|| anyhow!("the host presented no certificate"))?;
    let handle = session.handle();
    let (mut tx, mut rx, video) = session.split();
    tx.send(&ControlMessage::Hello {
        version: PROTOCOL_VERSION,
        viewer_name: computer_name(),
        pin,
    })
    .await?;
    let answer = tokio::time::timeout(WELCOME_TIMEOUT, rx.recv())
        .await
        .map_err(|_| ViewerFailure::no_answer())?
        .map_err(|e| ViewerFailure::ConnectionLost(format!("{e:#}")))?;
    match answer {
        Some(ControlMessage::Welcome {
            host_name,
            stream,
            input_allowed,
        }) => Ok(Connected {
            host_name,
            stream,
            input_allowed,
            fingerprint,
            tx,
            rx,
            video,
            handle,
        }),
        Some(ControlMessage::Rejected { reason }) => Err(ViewerFailure::Rejected(reason).into()),
        Some(other) => bail!("unexpected answer from the host: {other:?}"),
        None => Err(ViewerFailure::HostLeft.into()),
    }
}

/// Moves datagrams from the network to the render thread.
async fn receive_video(video: VideoChannel, out: std::sync::mpsc::Sender<Bytes>) {
    while let Ok(datagram) = video.recv().await {
        if out.send(datagram).is_err() {
            break; // render thread is gone
        }
    }
}

/// Sends our requests and watches for the host leaving.
async fn run_control(
    mut tx: ControlSender,
    mut rx: ControlReceiver,
    mut outgoing: UnboundedReceiver<ControlMessage>,
    window: EventLoopProxy<UserEvent>,
) {
    let reason = loop {
        tokio::select! {
            message = outgoing.recv() => {
                let Some(message) = message else { return };
                let is_bye = message == ControlMessage::Bye;
                if let Err(e) = tx.send(&message).await {
                    break ViewerFailure::ConnectionLost(format!("{e:#}"));
                }
                if is_bye {
                    return;
                }
            }
            incoming = rx.recv() => match incoming {
                Ok(Some(ControlMessage::Bye) | None) => break ViewerFailure::HostLeft,
                Ok(Some(ControlMessage::Cursor(shape))) => {
                    let _ = window.send_event(UserEvent::Cursor(shape));
                }
                Ok(Some(ControlMessage::CursorVisible(visible))) => {
                    let _ = window.send_event(UserEvent::CursorVisible(visible));
                }
                Ok(Some(ControlMessage::InputAllowed(allowed))) => {
                    let _ = window.send_event(UserEvent::InputAllowed(allowed));
                }
                Ok(Some(_)) => {}
                Err(e) => break ViewerFailure::ConnectionLost(format!("{e:#}")),
            }
        }
    };
    let _ = window.send_event(UserEvent::HostLeft(reason));
}

/// Asks for the PIN on the terminal; `None` if nothing was typed (or there
/// is no terminal: stdin then reads as empty).
fn ask_pin(question: &str) -> Result<Option<String>> {
    print!("{question}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(non_empty(line))
}

fn non_empty(pin: String) -> Option<String> {
    let pin = pin.trim();
    (!pin.is_empty()).then(|| pin.to_string())
}

/// Shared with the render thread so the window can request keyframes.
pub type Outgoing = UnboundedSender<ControlMessage>;

/// The PIN from `--pin`, else from [`PIN_ENV`]; `None`: none given.
fn given_pin(flag: Option<String>, env: Option<String>) -> Option<String> {
    flag.or(env).and_then(non_empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Option<String> {
        Some(text.to_string())
    }

    #[test]
    fn pin_flag_wins_over_environment() {
        assert_eq!(given_pin(s("111111"), s("222222")), s("111111"));
    }

    #[test]
    fn pin_from_environment_when_flag_absent() {
        assert_eq!(given_pin(None, s("222222")), s("222222"));
    }

    #[test]
    fn blank_pins_count_as_none() {
        assert_eq!(given_pin(None, None), None);
        assert_eq!(given_pin(None, s("  ")), None);
        assert_eq!(given_pin(s(""), None), None);
        assert_eq!(given_pin(s(" 42 "), None), s("42"));
    }

    #[test]
    fn remembered_hosts_are_tried_without_a_pin() {
        assert_eq!(first_attempt(None, true), FirstAttempt::WithoutPin);
        assert_eq!(first_attempt(None, false), FirstAttempt::AskPin);
    }

    #[test]
    fn a_given_pin_is_used_at_once() {
        for remembered in [true, false] {
            assert_eq!(
                first_attempt(s("123456"), remembered),
                FirstAttempt::Pin("123456".into())
            );
        }
    }

    #[test]
    fn only_pin_required_triggers_the_prompt() {
        let failure = |reason| anyhow::Error::new(ViewerFailure::Rejected(reason));
        assert!(is_pin_required(&failure(RejectReason::PinRequired)));
        assert!(!is_pin_required(&failure(RejectReason::WrongPin)));
        assert!(!is_pin_required(&anyhow!("PIN required")));
    }
}
