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
//! - tokio runtime: control channel and datagram reception; when the
//!   connection breaks by itself it also reconnects to the same host (see
//!   `control` and `reconnect`) while the window stays open;
//! - clipboard thread: listens for clipboard changes and holds the host's
//!   copied files as virtual files (an OLE apartment, see `clipboard`);
//! - render thread: owns the GPU objects, runs `ViewerStreamer`
//!   (reassemble -> decode -> present), fed through a bounded queue
//!   (see `feed`); it also reports to the host how the video arrives, for
//!   the host's bitrate adaptation (see `rate`).
//! - audio thread: plays the host's sound, which the network task takes out
//!   of the datagrams into a jitter buffer (see `audio`).
//!
//! Files dropped on the window or chosen in its "File" menu go to the host
//! (see `transfers`, `window_files`); the host's land in this user's
//! `Downloads\wshare` or the folder given with `--downloads`. Launched by
//! the desktop interface, the viewer also takes file commands on stdin
//! (see `channel`).

mod audio;
mod bar_menu;
pub mod channel;
mod clipboard;
mod control;
mod control_policy;
mod cursor;
pub mod failure;
mod feed;
mod input_map;
mod key_log;
mod key_router;
mod keyboard_hook;
mod live_handle;
mod notice;
mod outbox;
mod pairing;
mod rate;
mod reconnect;
mod remote;
mod render_thread;
pub mod report;
mod sink;
mod stall;
mod stats;
mod stream_requests;
mod toolbar;
mod transfers;
mod window;
mod window_files;

use std::io::Write;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use discovery::computer_name;
use net::{
    ControlReceiver, ControlSender, FileChannel, Fingerprint, Identity, SessionHandle, VideoChannel,
};
use protocol::control::{ControlMessage, PROTOCOL_VERSION, RejectReason, StreamInfo};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use winit::event_loop::{DeviceEvents, EventLoop};

use crate::cli::ViewOptions;
use crate::clipboard::ViewerRules;
use crate::options::DEFAULT_PORT;
use crate::target::{self, Resolved, parse_target};
use audio::{AudioIn, AudioPlayer};
use clipboard::ViewerClipboard;
use control::{ControlLink, ControlTask, Reconnection, VideoFeed};
use control_policy::ControlPolicy;
use failure::ViewerFailure;
use feed::VideoIn;
use live_handle::LiveHandle;
use pairing::Pairing;
use rate::TargetRate;
use report::{REPORT_ENV, ViewerReport};
use stats::LinkProbe;
use transfers::TransferSetup;
use window::{HostRequests, UserEvent, ViewerWindow, WindowOptions};

/// Environment variable holding the PIN when `--pin` is absent (used by
/// the desktop interface, so the PIN is not visible on the command line).
pub const PIN_ENV: &str = "WSHARE_PIN";

const DISCOVERY_WAIT: Duration = Duration::from_secs(3);
const WELCOME_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a viewer told to wait for the person at the host keeps waiting:
/// the time the host gives them, plus room for its own delays.
const APPROVAL_WAIT: Duration = Duration::from_secs(40);
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
    files: FileChannel,
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
    let connected = authenticate(&runtime, addr, &label, &pairing, given.clone(), reporting)?;
    let Connected {
        host_name,
        stream,
        input_allowed,
        fingerprint,
        tx,
        rx,
        video,
        files,
        handle,
    } = connected;
    // Before the "Connected" line: the desktop interface re-reads the known
    // hosts when it sees that line.
    pairing.remember(fingerprint, &host_name, addr);
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
    let (datagram_tx, datagram_rx) = feed::queue();
    let (outgoing_tx, outgoing_rx) = unbounded_channel::<ControlMessage>();
    let handle = LiveHandle::new(handle);
    let restart = feed::Restart::default();
    let audio_in = AudioIn::new();
    // Plays until the end of the session; without it the session goes on
    // silent.
    let _player = AudioPlayer::start(audio_in.clone())
        .inspect_err(|e| eprintln!("warning: the host's sound cannot be played: {e:#}"))
        .ok();
    let forward = runtime.spawn(feed::forward(video, datagram_tx.clone(), audio_in.clone()));
    let target = TargetRate::default();
    let rules = ViewerRules {
        host_allows: input_allowed,
        view_only: options.view_only,
    };
    // Opened only now, with the session: nothing is shared before or after it.
    let clipboard = ViewerClipboard::open(rules);
    let (transfers, commands_tx, commands_rx) = TransferSetup::new(
        options.downloads.clone(),
        rules,
        event_loop.create_proxy(),
        reporting,
    );
    let control = runtime.spawn(control::run(ControlTask {
        link: ControlLink { tx, rx, files },
        outgoing: outgoing_rx,
        window: event_loop.create_proxy(),
        target: target.clone(),
        clipboard,
        commands: commands_rx,
        transfers,
        reconnection: Reconnection {
            addr,
            host: fingerprint,
            stream: stream.clone(),
            identity: pairing.into_identity(),
            pin: given,
        },
        feed: VideoFeed {
            forward,
            datagrams: datagram_tx,
            audio: audio_in,
            restart: restart.clone(),
            handle: handle.clone(),
        },
    }));

    let rtt_handle = handle.clone();
    let mut window = ViewerWindow::new(
        WindowOptions {
            title: format!("{host_name} - wshare"),
            fullscreen: options.fullscreen,
            stats: options.stats,
        },
        stream,
        VideoIn {
            datagrams: datagram_rx,
            restart,
        },
        LinkProbe {
            rtt: Box::new(move || rtt_handle.rtt()),
            target,
        },
        HostRequests {
            control: outgoing_tx.clone(),
            files: commands_tx,
        },
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
            "Session: {} frames shown, {} lost, {} recovered ({} resend requests), \
             {} skipped, {} passed over to catch up, {} keyframes requested",
            stats.frames_presented,
            stats.frames_lost,
            stats.frames_recovered,
            stats.nacks_sent,
            stats.frames_skipped,
            stats.frames_superseded,
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
    reporting: bool,
) -> Result<Connected> {
    let mut pin = match first_attempt(given, pairing.expected().is_some()) {
        FirstAttempt::Pin(pin) => Some(pin),
        FirstAttempt::WithoutPin => None,
        FirstAttempt::AskPin => prompt_pin(
            &format!("PIN shown on {label} (empty: ask the host to accept this PC): "),
            reporting,
        )?,
    };
    loop {
        match runtime.block_on(connect(
            addr,
            pairing.expected(),
            pairing.identity(),
            pin.clone(),
            || announce_waiting(reporting),
        )) {
            Err(e) if pin.is_none() && requested_credential(&e).is_some() => {
                let question = match requested_credential(&e) {
                    Some(Credential::Password) => {
                        format!("{label} requires the password of its unattended access: ")
                    }
                    _ => format!("{label} requires the PIN: "),
                };
                pin = prompt_pin(&question, reporting)?;
                if pin.is_none() {
                    return Err(e);
                }
            }
            result => return result,
        }
    }
}

/// What a host that refused us for want of one asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Credential {
    Pin,
    /// The password of the host's unattended access (sent as the PIN).
    Password,
}

/// The credential the host refused `error` for not having, if any.
fn requested_credential(error: &anyhow::Error) -> Option<Credential> {
    match error.downcast_ref::<ViewerFailure>()? {
        ViewerFailure::Rejected(RejectReason::PinRequired) => Some(Credential::Pin),
        ViewerFailure::Rejected(RejectReason::PasswordRequired) => Some(Credential::Password),
        _ => None,
    }
}

/// One connection attempt: TLS (pinned for a remembered host, presenting
/// our certificate), then Hello and the host's answer.
async fn connect(
    addr: SocketAddr,
    expected: Option<Fingerprint>,
    identity: Option<&Identity>,
    pin: Option<String>,
    awaiting_approval: impl FnOnce(),
) -> Result<Connected> {
    let session = net::connect(addr, expected, identity)
        .await
        .map_err(failure::connect_failure)?;
    let fingerprint = session
        .peer_fingerprint()
        .ok_or_else(|| anyhow!("the host presented no certificate"))?;
    let handle = session.handle();
    let (mut tx, mut rx, video, files) = session.split();
    tx.send(&ControlMessage::Hello {
        version: PROTOCOL_VERSION,
        viewer_name: computer_name(),
        pin,
    })
    .await
    .map_err(|e| ViewerFailure::ConnectionLost(format!("{e:#}")))?;
    let mut answer = next_answer(&mut rx, WELCOME_TIMEOUT).await?;
    if answer == Some(ControlMessage::AwaitingApproval) {
        // The person at the host is asked; Welcome or Rejected follows.
        awaiting_approval();
        answer = next_answer(&mut rx, APPROVAL_WAIT).await?;
    }
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
            files,
            handle,
        }),
        Some(ControlMessage::Rejected { reason }) => Err(ViewerFailure::Rejected(reason).into()),
        Some(other) => bail!("unexpected answer from the host: {other:?}"),
        None => Err(ViewerFailure::HostLeft.into()),
    }
}

/// The host's next message of the handshake, within `within`.
async fn next_answer(
    rx: &mut ControlReceiver,
    within: Duration,
) -> Result<Option<ControlMessage>, ViewerFailure> {
    tokio::time::timeout(within, rx.recv())
        .await
        .map_err(|_| ViewerFailure::no_answer())?
        .map_err(|e| ViewerFailure::ConnectionLost(format!("{e:#}")))
}

/// Says the host asked the person at its PC about this one: in a line for
/// the desktop interface, when it wants them, and for terminals.
fn announce_waiting(reporting: bool) {
    if reporting {
        println!("{}", ViewerReport::AwaitingApproval.to_line());
    }
    println!("Waiting for the host to accept this PC...");
}

/// Asks for the PIN on the terminal; `None` without asking when the desktop
/// interface launched the viewer (`reporting`): its stdin carries commands,
/// never a PIN (see `channel`).
fn prompt_pin(question: &str, reporting: bool) -> Result<Option<String>> {
    if !may_prompt_pin(reporting) {
        return Ok(None);
    }
    ask_pin(question)
}

/// Only a terminal user is asked for the PIN.
fn may_prompt_pin(reporting: bool) -> bool {
    !reporting
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
    fn a_viewer_launched_by_the_interface_never_reads_a_pin_from_stdin() {
        assert!(may_prompt_pin(false), "a terminal is asked");
        assert!(
            !may_prompt_pin(true),
            "the interface's stdin carries commands"
        );
        assert_eq!(prompt_pin("PIN: ", true).unwrap(), None);
    }

    #[test]
    fn only_a_missing_pin_or_password_triggers_the_prompt() {
        let failure = |reason| anyhow::Error::new(ViewerFailure::Rejected(reason));
        let asked = |reason| requested_credential(&failure(reason));
        assert_eq!(asked(RejectReason::PinRequired), Some(Credential::Pin));
        assert_eq!(
            asked(RejectReason::PasswordRequired),
            Some(Credential::Password)
        );
        assert_eq!(asked(RejectReason::WrongPin), None);
        assert_eq!(asked(RejectReason::WrongPassword), None);
        assert_eq!(asked(RejectReason::NotAuthorized), None);
        assert_eq!(requested_credential(&anyhow!("PIN required")), None);
    }
}
