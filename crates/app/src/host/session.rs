//! One viewer, from Hello to goodbye: admission (see [`super::admission`]),
//! stream setup, Welcome and the monitor list, then the control loop
//! (keyframe requests, reception reports, remote input, the viewer's choice
//! of monitor and quality, pointer, bitrate and stream updates) until the
//! viewer, the stream or the host ends it. A viewer admitted with the PIN
//! and a certificate is authorized from then on.
//!
//! Every outcome is reported as a [`HostEvent`]; nothing here prints.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use capture::MonitorInfo;
use input::RemoteInput;
use net::{ControlReceiver, ControlSender, FileChannel, Session, SessionHandle, VideoChannel};
use pipeline::HostStats;
use protocol::control::{ControlMessage, RejectReason, StreamInfo};
use protocol::transfer::TransferMessage;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::sync::watch;

use super::admission::{Admission, Applicant, admit, send_rejection, viewer_trust};
use super::approvals::Approvals;
use super::clipboard::{ClipboardSettings, SessionClipboard};
use super::devices::Devices;
use super::events::EventSink;
use super::input_gate::InputGate;
use super::input_switch::InputSwitch;
use super::monitor_list::{area, find};
use super::pin_guard::PinGuard;
use super::screen_choice::{ScreenChoice, start_monitor};
use super::screen_switch::{InputTarget, SessionScreens};
use super::state::{Cancelled, SessionTicket, SharedState};
use super::streaming::{StreamEnd, StreamPlan, StreamUpdate, StreamingThread};
use super::transfers::SessionTransfers;
use super::{HostEvent, HostWarning, SessionFailure};
use crate::options::StreamOptions;

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
    /// Where the files the viewer sends land.
    pub(super) downloads: PathBuf,
    /// The monitor sessions start on: the one in `stream` until a viewer
    /// picks another, which the next viewers then get too.
    pub(super) monitor: AtomicUsize,
    pub(super) state: Arc<SharedState>,
    pub(super) devices: Arc<Devices>,
    pub(super) approvals: Arc<Approvals>,
    pub(super) pin_guard: PinGuard,
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

/// Serves one viewer to the end, within the session `ticket` registered.
pub(super) async fn serve_viewer(session: Session, ctx: &SessionContext, ticket: &SessionTicket) {
    let mut cancel = ticket.cancelled();
    let address = session.remote_addr();
    let handle = session.handle();
    // Proven by the TLS handshake; `None` if the viewer has no certificate.
    let certificate = session.peer_fingerprint();
    let (mut tx, mut rx, video, files) = session.split();
    let applicant = Applicant {
        address,
        trust: viewer_trust(certificate.as_ref(), &ctx.devices),
        handle: &handle,
    };

    match admit(&mut tx, &mut rx, ctx, applicant, ticket, &mut cancel).await {
        Admission::Admitted {
            viewer_name,
            remember,
        } => {
            ctx.devices
                .record_admitted(certificate, &viewer_name, remember);
            let viewer = Viewer {
                address,
                name: viewer_name,
                handle: handle.clone(),
            };
            let channels = Channels {
                tx,
                rx,
                video,
                files,
            };
            serve_admitted(viewer, channels, ctx, cancel).await;
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
    files: FileChannel,
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
        files,
    } = channels;
    let address = viewer.address;
    let refuse = async |tx: &mut ControlSender, reason: RejectReason| {
        send_rejection(tx, reason.clone()).await;
        ctx.events
            .emit(HostEvent::ViewerRejected { address, reason });
    };

    let (updates_tx, mut updates_rx) = unbounded_channel();
    let prepared = async {
        // Looked up per session: the monitor layout may change while the
        // service runs.
        let monitors = capture::list_monitors()?;
        let first = start_monitor(
            ctx.monitor.load(Ordering::Relaxed),
            ctx.stream.monitor,
            monitors.len(),
        );
        let screens = SessionScreens::new(first);
        let input = RemoteInput::new(area(find(&monitors, screens.current().monitor)?));
        let plan = StreamPlan {
            base: ctx.stream.clone(),
            choice: screens.current(),
        };
        let mut streaming = StreamingThread::start(plan, video, updates_tx, ctx.events.clone())?;
        let info = streaming.ready().await?;
        anyhow::Ok((monitors, screens, input, streaming, info))
    };
    let (monitors, screens, input, streaming, info) = match prepared.await {
        Ok(parts) => parts,
        Err(e) => {
            refuse(&mut tx, RejectReason::CannotStream(format!("{e:#}"))).await;
            farewell(&viewer.handle, &mut cancel).await;
            return;
        }
    };
    if cancel.is_cancelled() {
        refuse(
            &mut tx,
            RejectReason::Other("the host stopped sharing".into()),
        )
        .await;
        let _ = streaming.stop().await;
        return;
    }
    // Subscribed before reading the flag for Welcome, so a change made
    // right after it is not missed.
    let mut allowed = ctx.state.watch_input();
    let told = *allowed.borrow_and_update();
    let welcome = welcome(&ctx.host_name, info, told);
    let sent = async {
        tx.send(&welcome).await?;
        tx.send(&screens.list(&monitors)).await
    };
    if let Err(e) = sent.await {
        let reason = RejectReason::Other(format!("viewer left before Welcome: {e:#}"));
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
    let settings = ClipboardSettings {
        sharing: ctx.state.watch_clipboard(),
        receive_files: ctx.state.watch_receive_files(),
    };
    let clipboard = SessionClipboard::open(settings, ctx.events.clone());
    let transfers = SessionTransfers::start(files, ctx);
    let loop_state = ControlLoop {
        tx: &mut tx,
        rx: &mut rx,
        updates: &mut updates_rx,
        streaming: &streaming,
        gate: &mut gate,
        allowed,
        switch: InputSwitch::new(told),
        clipboard,
        transfers,
        screens,
        monitor: &ctx.monitor,
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

/// Gives our last message time to arrive: waits (bounded) for the viewer
/// to close the connection after reading it.
async fn farewell(handle: &SessionHandle, cancel: &mut Cancelled) {
    tokio::select! {
        _ = tokio::time::timeout(FAREWELL_GRACE, handle.closed()) => {}
        () = cancel.wait() => {}
    }
}

/// Whether the session ended abnormally on our side, and why.
fn session_error(
    ended: Result<SessionEnd>,
    stream_error: Option<anyhow::Error>,
) -> Option<SessionFailure> {
    let failure = |e: anyhow::Error| SessionFailure(format!("{e:#}"));
    match (ended, stream_error) {
        (Err(e), _) => Some(failure(e)),
        // Sends fail once the viewer has gone: that is a normal end.
        (Ok(SessionEnd::ViewerLeft), _) | (Ok(_), None) => None,
        (Ok(SessionEnd::StreamStopped | SessionEnd::HostEnded), Some(e)) => Some(failure(e)),
    }
}

/// Everything the control loop drives.
struct ControlLoop<'a> {
    tx: &'a mut ControlSender,
    rx: &'a mut ControlReceiver,
    /// What the streaming thread has for the viewer (pointer, bitrate) and
    /// for the session (the stream started over).
    updates: &'a mut UnboundedReceiver<StreamUpdate>,
    streaming: &'a StreamingThread,
    gate: &'a mut InputGate<RemoteInput>,
    allowed: watch::Receiver<bool>,
    switch: InputSwitch,
    /// The text and files copied on either PC, while the session lasts.
    clipboard: SessionClipboard,
    /// The files sent, dropped or pasted, both ways.
    transfers: SessionTransfers,
    /// The monitor and quality the viewer chose.
    screens: SessionScreens,
    /// Where the monitor chosen is kept for the next sessions.
    monitor: &'a AtomicUsize,
    cancel: &'a mut Cancelled,
    handle: &'a SessionHandle,
    events: &'a EventSink,
}

impl ControlLoop<'_> {
    /// Serves the viewer until either side stops: forwards its requests and
    /// input, and sends it our pointer and bitrate updates.
    async fn run(mut self) -> Result<SessionEnd> {
        loop {
            tokio::select! {
                message = self.rx.recv() => match message? {
                    Some(ControlMessage::RequestKeyframe) => self.streaming.request_keyframe(),
                    Some(ControlMessage::Nack(nack)) => self.streaming.resend(&nack)?,
                    Some(ControlMessage::ReceptionReport(report)) => {
                        self.streaming.record_report(&report);
                    }
                    Some(ControlMessage::Input(event)) => {
                        let allowed = *self.allowed.borrow();
                        self.report(|gate, now| gate.apply(allowed, &event, now));
                    }
                    Some(ControlMessage::Clipboard(text)) => {
                        let allowed = *self.allowed.borrow();
                        self.clipboard.remote_text(&text, allowed).await;
                    }
                    Some(ControlMessage::Transfer(message)) => self.on_transfer(message),
                    // Watching is choosing too: allowed without remote control.
                    Some(ControlMessage::SelectMonitor(index)) => {
                        let monitors = self.monitors();
                        let choice = self.screens.select_monitor(index, &monitors);
                        self.reconfigure(choice);
                    }
                    Some(ControlMessage::SetQuality(quality)) => {
                        let choice = self.screens.set_quality(quality);
                        self.reconfigure(choice);
                    }
                    Some(ControlMessage::Bye) | None => return Ok(SessionEnd::ViewerLeft),
                    Some(_) => {} // ignore unexpected messages
                },
                Some(update) = self.updates.recv() => self.forward(update).await?,
                message = self.transfers.next() => self.tx.send(&message).await?,
                () = self.clipboard.changed() => {
                    let allowed = *self.allowed.borrow();
                    let files = &self.transfers.hub;
                    if let Some(message) = self.clipboard.local_change(allowed, files).await {
                        self.tx.send(&message).await?;
                    }
                }
                Ok(()) = self.allowed.changed() => {
                    let allowed = *self.allowed.borrow_and_update();
                    let actions = self.switch.update(allowed);
                    if actions.release {
                        self.report(|gate, now| gate.release_all(now));
                    }
                    if let Some(message) = actions.notify {
                        self.tx.send(&message).await?;
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

    /// A step of a file transfer: the viewer's copied files go to the
    /// clipboard, the rest to the transfers.
    fn on_transfer(&mut self, message: TransferMessage) {
        match message {
            TransferMessage::ClipboardFiles { offer, entries } => {
                let allowed = *self.allowed.borrow();
                let files = &self.transfers.hub;
                self.clipboard.remote_files(offer, entries, allowed, files);
            }
            other => self.transfers.hub.on_message(other),
        }
    }

    /// The host's monitors now. If Windows cannot list them, that is
    /// reported and none are known: a choice cannot be checked then, and
    /// remote input lets go rather than aim at a stale area.
    fn monitors(&self) -> Vec<MonitorInfo> {
        capture::list_monitors().unwrap_or_else(|e| {
            let warning = HostWarning::ScreenUnavailable(format!("{e:#}"));
            self.events.emit(HostEvent::Warning(warning));
            Vec::new()
        })
    }

    /// Has the streaming thread start over for `choice`, if there is one.
    fn reconfigure(&self, choice: Option<ScreenChoice>) {
        if let Some(choice) = choice {
            self.streaming.reconfigure(choice);
        }
    }

    /// Passes on what the streaming thread has: to the viewer as it is, or,
    /// when the stream started over, the new stream and monitor list, with
    /// remote input aimed at the screen now shared.
    async fn forward(&mut self, update: StreamUpdate) -> Result<()> {
        let (choice, info) = match update {
            StreamUpdate::Viewer(message) => return self.tx.send(&message).await,
            StreamUpdate::Restarted { choice, info } => (choice, info),
        };
        // The layout may have changed with the mode: looked up now.
        let monitors = self.monitors();
        let switched = self.screens.restarted(choice, info, &monitors);
        self.monitor.store(choice.monitor, Ordering::Relaxed);
        match switched.input {
            InputTarget::Aim {
                area,
                other_monitor,
            } => self.report(|gate, now| gate.retarget(area, other_monitor, now)),
            InputTarget::Unknown => self.report(|gate, now| gate.release_all(now)),
        }
        if let Some(event) = switched.event {
            self.events.emit(event);
        }
        for message in &switched.messages {
            self.tx.send(message).await?;
        }
        Ok(())
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
        assert_eq!(error, Some(SessionFailure("encoder lost".into())));
        let error = session_error(Ok(SessionEnd::HostEnded), Some(anyhow!("encoder lost")));
        assert_eq!(error, Some(SessionFailure("encoder lost".into())));
    }

    #[test]
    fn control_channel_errors_win() {
        let error = session_error(Err(anyhow!("reset")), Some(anyhow!("encoder lost")));
        assert_eq!(error, Some(SessionFailure("reset".into())));
    }

    #[test]
    fn welcome_carries_the_input_flag_of_connect_time() {
        let info = StreamInfo {
            codec: protocol::control::VideoCodec::H264,
            width: 1920,
            height: 1080,
            fps: 60,
            encoder: "Test Encoder".into(),
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
