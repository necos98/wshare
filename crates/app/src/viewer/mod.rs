//! `wshare view`: connect to a host and show its screen.
//!
//! Threads:
//! - main thread: the window (winit requires it);
//! - tokio runtime: control channel and datagram reception;
//! - render thread: owns the GPU objects, runs `ViewerStreamer`
//!   (reassemble -> decode -> present).

mod cursor;
mod input_map;
mod remote;
mod render_thread;
mod sink;
mod window;

use std::io::Write;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use discovery::computer_name;
use net::{ControlReceiver, ControlSender, Fingerprint, SessionHandle, VideoChannel};
use protocol::control::{ControlMessage, PROTOCOL_VERSION, StreamInfo};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use winit::event_loop::{EventLoop, EventLoopProxy};

use crate::cli::{DEFAULT_PORT, ViewOptions};
use crate::target::{self, parse_target};
use window::{UserEvent, ViewerWindow};

const DISCOVERY_WAIT: Duration = Duration::from_secs(3);
const WELCOME_TIMEOUT: Duration = Duration::from_secs(10);
/// Time given to the Bye message to leave before the connection closes.
const GOODBYE_GRACE: Duration = Duration::from_millis(300);

/// A session that passed the handshake.
struct Connected {
    host_name: String,
    stream: StreamInfo,
    input_allowed: bool,
    fingerprint: Option<Fingerprint>,
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
    let (addr, label) = target::resolve(&target, DISCOVERY_WAIT)?;
    let pin = match options.pin {
        Some(pin) => pin,
        None => prompt(&format!("PIN shown on {label}: "))?,
    };

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let connected = runtime.block_on(connect(addr, pin))?;
    let Connected { host_name, stream, input_allowed, fingerprint, tx, rx, video, handle } =
        connected;
    println!(
        "Connected to \"{host_name}\" ({addr}) - {}x{} @ {} fps {:?}",
        stream.width, stream.height, stream.fps, stream.codec
    );
    if let Some(fp) = fingerprint {
        println!("Host fingerprint: {fp}");
    }
    let control_input = input_allowed && !options.view_only;
    if !control_input {
        println!("View only: mouse and keyboard stay local.");
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let (datagram_tx, datagram_rx) = std::sync::mpsc::channel::<Bytes>();
    let (outgoing_tx, outgoing_rx) = unbounded_channel::<ControlMessage>();
    runtime.spawn(receive_video(video, datagram_tx));
    let control = runtime.spawn(run_control(tx, rx, outgoing_rx, event_loop.create_proxy()));

    let mut window = ViewerWindow::new(
        format!("{host_name} - wshare"),
        stream,
        datagram_rx,
        outgoing_tx.clone(),
        control_input,
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
            stats.frames_presented, stats.frames_lost, stats.frames_skipped, stats.keyframes_requested
        );
    }
    window.error().map_or(Ok(()), |e| bail!("{e}"))
}

async fn connect(addr: SocketAddr, pin: String) -> Result<Connected> {
    let session = net::connect(addr, None).await?;
    let fingerprint = session.peer_fingerprint();
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
        .context("the host did not answer")??;
    match answer {
        Some(ControlMessage::Welcome { host_name, stream, input_allowed }) => Ok(Connected {
            host_name,
            stream,
            input_allowed,
            fingerprint,
            tx,
            rx,
            video,
            handle,
        }),
        Some(ControlMessage::Rejected { reason }) => bail!("the host refused: {reason}"),
        Some(other) => bail!("unexpected answer from the host: {other:?}"),
        None => bail!("the host closed the connection"),
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
                if tx.send(&message).await.is_err() {
                    break "connection lost".to_string();
                }
                if is_bye {
                    return;
                }
            }
            incoming = rx.recv() => match incoming {
                Ok(Some(ControlMessage::Bye) | None) => break "the host stopped sharing".to_string(),
                Ok(Some(ControlMessage::Cursor(shape))) => {
                    let _ = window.send_event(UserEvent::Cursor(shape));
                }
                Ok(Some(ControlMessage::CursorVisible(visible))) => {
                    let _ = window.send_event(UserEvent::CursorVisible(visible));
                }
                Ok(Some(_)) => {}
                Err(e) => break format!("connection lost: {e:#}"),
            }
        }
    };
    let _ = window.send_event(UserEvent::HostLeft(reason));
}

fn prompt(question: &str) -> Result<String> {
    print!("{question}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Shared with the render thread so the window can request keyframes.
pub type Outgoing = UnboundedSender<ControlMessage>;
