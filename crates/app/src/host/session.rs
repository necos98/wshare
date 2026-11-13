//! One viewer, from Hello to goodbye: admission (PIN), stream setup,
//! Welcome, then the control loop (keyframe requests, remote input, pointer
//! updates) until the viewer, the stream or the host ends it.
//!
//! Every outcome is reported as a [`HostEvent`]; nothing here prints.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use input::{RemoteInput, ScreenArea};
use net::{ControlReceiver, ControlSender, Session, SessionHandle, VideoChannel};
use pipeline::HostStats;
use protocol::control::{ControlMessage, StreamInfo};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::sync::watch;

use super::HostEvent;
use super::events::EventSink;
use super::input_gate::InputGate;
use super::state::{Cancelled, SharedState};
use super::streaming::{StreamEnd, StreamingThread};
use crate::cli::StreamOptions;
use crate::handshake::{HelloVerdict, check_hello};

/// How long a connected viewer has to send Hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Delay after a wrong PIN, to make guessing slow.
const REJECT_DELAY: Duration = Duration::from_secs(1);
/// Grace period to tell "viewer left" from "our stream failed" when the
/// streaming thread stops first.
const LEAVE_GRACE: Duration = Duration::from_millis(300);
/// How long a last message (Bye, Rejected) gets to reach the viewer before
/// we close the connection, which would discard it if still in flight.
const FAREWELL_GRACE: Duration = Duration::from_secs(1);

/// What every session of one service shares.
pub(super) struct SessionContext {
    pub(super) host_name: String,
    pub(super) stream: StreamOptions,
    pub(super) state: Arc<SharedState>,
    pub(super) events: EventSink,
}

/// Who ended the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEnd {
    ViewerLeft,
    StreamStopped,
    /// `disconnect_viewer` or `stop` on our side.
    HostEnded,
}

/// Result of the Hello exchange.
enum Admission {
    Admitted {
        viewer_name: String,
    },
    Refused {
        reason: String,
    },
    /// The host ended the session before admission completed.
    Abandoned,
}

/// Serves one viewer to the end. `cancel` fires when the host wants the
/// session over (disconnect or stop).
pub(super) async fn serve_viewer(session: Session, ctx: &SessionContext, mut cancel: Cancelled) {
    let address = session.remote_addr();
    let handle = session.handle();
    let (mut tx, mut rx, video) = session.split();

    match admit(&mut tx, &mut rx, &ctx.state, &mut cancel).await {
        Admission::Admitted { viewer_name } => {
            let viewer = Viewer {
                address,
                name: viewer_name,
                handle: handle.clone(),
            };
            serve_admitted(viewer, Channels { tx, rx, video }, ctx, cancel).await;
        }
        Admission::Refused { reason } => {
            ctx.events
                .emit(HostEvent::ViewerRejected { address, reason });
            farewell(&handle, &mut cancel).await;
        }
        Admission::Abandoned => {}
    }
    handle.close("session over");
}

/// Waits for Hello and checks it against the PIN valid right now.
async fn admit(
    tx: &mut ControlSender,
    rx: &mut ControlReceiver,
    state: &SharedState,
    cancel: &mut Cancelled,
) -> Admission {
    let hello = tokio::select! {
        () = cancel.wait() => return Admission::Abandoned,
        hello = tokio::time::timeout(HELLO_TIMEOUT, rx.recv()) => hello,
    };
    let refused = |reason: &str| Admission::Refused {
        reason: reason.to_string(),
    };
    let hello = match hello {
        Err(_) => return refused("viewer sent no Hello"),
        Ok(Err(e)) => return refused(&format!("{e:#}")),
        Ok(Ok(None)) => return refused("viewer left before Hello"),
        Ok(Ok(Some(message))) => message,
    };
    match check_hello(&hello, &state.pin()) {
        HelloVerdict::Accept { viewer_name } => Admission::Admitted { viewer_name },
        HelloVerdict::Reject { reason } => {
            tokio::select! {
                () = cancel.wait() => return Admission::Abandoned,
                () = tokio::time::sleep(REJECT_DELAY) => {}
            }
            let _ = tx
                .send(&ControlMessage::Rejected {
                    reason: reason.clone(),
                })
                .await;
            let _ = tx.finish();
            Admission::Refused { reason }
        }
    }
}

/// An admitted viewer.
struct Viewer {
    address: SocketAddr,
    name: String,
    handle: SessionHandle,
}

/// The session's network channels.
struct Channels {
    tx: ControlSender,
    rx: ControlReceiver,
    video: VideoChannel,
}

/// Sets up streaming, sends Welcome, serves the viewer, then cleans up.
async fn serve_admitted(
    viewer: Viewer,
    channels: Channels,
    ctx: &SessionContext,
    mut cancel: Cancelled,
) {
    let Channels {
        mut tx,
        mut rx,
        video,
    } = channels;
    let address = viewer.address;
    let refuse = async |tx: &mut ControlSender, reason: String| {
        let _ = tx
            .send(&ControlMessage::Rejected {
                reason: reason.clone(),
            })
            .await;
        let _ = tx.finish();
        ctx.events
            .emit(HostEvent::ViewerRejected { address, reason });
    };

    let (cursor_tx, mut cursor_rx) = unbounded_channel();
    let prepared = async {
        // Looked up per session: the monitor layout may change while the
        // service runs.
        let input = remote_input(ctx.stream.monitor)?;
        let mut streaming = StreamingThread::start(ctx.stream.clone(), video, cursor_tx)?;
        let info = streaming.ready().await?;
        anyhow::Ok((input, streaming, info))
    };
    let (input, streaming, info) = match prepared.await {
        Ok(parts) => parts,
        Err(e) => {
            refuse(&mut tx, format!("host cannot stream: {e:#}")).await;
            farewell(&viewer.handle, &mut cancel).await;
            return;
        }
    };
    if cancel.is_cancelled() {
        refuse(&mut tx, "the host stopped sharing".into()).await;
        let _ = streaming.stop().await;
        return;
    }
    let welcome = welcome(&ctx.host_name, info, ctx.state.input_allowed());
    if let Err(e) = tx.send(&welcome).await {
        let reason = format!("viewer left before Welcome: {e:#}");
        ctx.events
            .emit(HostEvent::ViewerRejected { address, reason });
        let _ = streaming.stop().await;
        return;
    }
    ctx.events.emit(HostEvent::ViewerConnected {
        viewer_name: viewer.name,
        address,
    });
    streaming.go();

    let mut gate = InputGate::new(input);
    let loop_state = ControlLoop {
        tx: &mut tx,
        rx: &mut rx,
        cursor: &mut cursor_rx,
        streaming: &streaming,
        gate: &mut gate,
        allowed: ctx.state.watch_input(),
        cancel: &mut cancel,
        handle: &viewer.handle,
        events: &ctx.events,
    };
    let ended = loop_state.run().await;
    // Nothing the viewer pressed may stay pressed on the host.
    if let Some(event) = gate.release_all(Instant::now()) {
        ctx.events.emit(event);
    }
    drop(gate);
    viewer.handle.close("session over");
    let (stats, stream_error) = match streaming.stop().await {
        Ok(StreamEnd { stats, error }) => (stats, error),
        Err(e) => (HostStats::default(), Some(e)),
    };
    let error = session_error(ended, stream_error);
    ctx.events.emit(HostEvent::ViewerDisconnected {
        address,
        stats,
        error,
    });
}

fn welcome(host_name: &str, stream: StreamInfo, input_allowed: bool) -> ControlMessage {
    ControlMessage::Welcome {
        host_name: host_name.to_string(),
        stream,
        input_allowed,
    }
}

/// Injection into the shared monitor's area of the virtual desktop.
/// Creating it injects nothing.
fn remote_input(monitor: usize) -> Result<RemoteInput> {
    let info = capture::list_monitors()?
        .into_iter()
        .nth(monitor)
        .with_context(|| format!("monitor {monitor} not found"))?;
    Ok(RemoteInput::new(ScreenArea {
        left: info.left,
        top: info.top,
        width: info.width,
        height: info.height,
    }))
}

/// Gives our last message time to arrive: waits (bounded) for the viewer
/// to close the connection after reading it.
async fn farewell(handle: &SessionHandle, cancel: &mut Cancelled) {
    tokio::select! {
        _ = tokio::time::timeout(FAREWELL_GRACE, handle.closed()) => {}
        () = cancel.wait() => {}
    }
}

/// Whether the session ended abnormally on our side, and why.
fn session_error(ended: Result<SessionEnd>, stream_error: Option<anyhow::Error>) -> Option<String> {
    match (ended, stream_error) {
        (Err(e), _) => Some(format!("{e:#}")),
        // Sends fail once the viewer has gone: that is a normal end.
        (Ok(SessionEnd::ViewerLeft), _) | (Ok(_), None) => None,
        (Ok(SessionEnd::StreamStopped | SessionEnd::HostEnded), Some(e)) => Some(format!("{e:#}")),
    }
}

/// Everything the control loop drives.
struct ControlLoop<'a> {
    tx: &'a mut ControlSender,
    rx: &'a mut ControlReceiver,
    cursor: &'a mut UnboundedReceiver<ControlMessage>,
    streaming: &'a StreamingThread,
    gate: &'a mut InputGate<RemoteInput>,
    allowed: watch::Receiver<bool>,
    cancel: &'a mut Cancelled,
    handle: &'a SessionHandle,
    events: &'a EventSink,
}

impl ControlLoop<'_> {
    /// Serves the viewer until either side stops: forwards its requests and
    /// input, and sends it our pointer updates.
    async fn run(mut self) -> Result<SessionEnd> {
        loop {
            tokio::select! {
                message = self.rx.recv() => match message? {
                    Some(ControlMessage::RequestKeyframe) => self.streaming.request_keyframe(),
                    Some(ControlMessage::Input(event)) => {
                        let allowed = *self.allowed.borrow();
                        self.report(|gate, now| gate.apply(allowed, &event, now));
                    }
                    Some(ControlMessage::Bye) | None => return Ok(SessionEnd::ViewerLeft),
                    Some(_) => {} // ignore unexpected messages
                },
                Some(update) = self.cursor.recv() => self.tx.send(&update).await?,
                Ok(()) = self.allowed.changed() => {
                    if !*self.allowed.borrow_and_update() {
                        self.report(|gate, now| gate.release_all(now));
                    }
                }
                () = self.cancel.wait() => {
                    self.say_bye().await;
                    return Ok(SessionEnd::HostEnded);
                }
                () = self.streaming.finished() => {
                    // The thread usually stops because the viewer just closed
                    // the connection; its Bye (or the close) is then already
                    // here.
                    return Ok(match tokio::time::timeout(LEAVE_GRACE, self.rx.recv()).await {
                        Ok(Ok(Some(ControlMessage::Bye) | None) | Err(_)) => SessionEnd::ViewerLeft,
                        _ => {
                            let _ = self.tx.send(&ControlMessage::Bye).await;
                            SessionEnd::StreamStopped
                        }
                    });
                }
            }
        }
    }

    fn report(
        &mut self,
        act: impl FnOnce(&mut InputGate<RemoteInput>, Instant) -> Option<HostEvent>,
    ) {
        if let Some(event) = act(self.gate, Instant::now()) {
            self.events.emit(event);
        }
    }

    /// Tells the viewer we are ending the session and lets the Bye arrive.
    async fn say_bye(&mut self) {
        if self.tx.send(&ControlMessage::Bye).await.is_ok() {
            let _ = self.tx.finish();
            let _ = tokio::time::timeout(FAREWELL_GRACE, self.handle.closed()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    #[test]
    fn normal_ends_report_no_error() {
        assert_eq!(session_error(Ok(SessionEnd::ViewerLeft), None), None);
        assert_eq!(session_error(Ok(SessionEnd::StreamStopped), None), None);
        assert_eq!(session_error(Ok(SessionEnd::HostEnded), None), None);
    }

    #[test]
    fn stream_failures_after_the_viewer_left_are_not_errors() {
        assert_eq!(
            session_error(Ok(SessionEnd::ViewerLeft), Some(anyhow!("send failed"))),
            None
        );
    }

    #[test]
    fn stream_failures_while_the_viewer_watches_are_errors() {
        let error = session_error(Ok(SessionEnd::StreamStopped), Some(anyhow!("encoder lost")));
        assert_eq!(error.as_deref(), Some("encoder lost"));
        let error = session_error(Ok(SessionEnd::HostEnded), Some(anyhow!("encoder lost")));
        assert_eq!(error.as_deref(), Some("encoder lost"));
    }

    #[test]
    fn control_channel_errors_win() {
        let error = session_error(Err(anyhow!("reset")), Some(anyhow!("encoder lost")));
        assert_eq!(error.as_deref(), Some("reset"));
    }

    #[test]
    fn welcome_carries_the_input_flag_of_connect_time() {
        let info = StreamInfo {
            codec: protocol::control::VideoCodec::H264,
            width: 1920,
            height: 1080,
            fps: 60,
        };
        let ControlMessage::Welcome {
            input_allowed,
            host_name,
            ..
        } = welcome("desk", info, false)
        else {
            panic!()
        };
        assert!(!input_allowed);
        assert_eq!(host_name, "desk");
    }
}
