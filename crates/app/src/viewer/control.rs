//! The viewer's control task: sends our requests, handles what the host
//! tells us and, when the connection breaks by itself, gets the session back.
//!
//! A break (see [`reconnect::is_retryable`]) does not close the window: the
//! picture stays, a message says the viewer is reconnecting, and the same
//! host (pinned by fingerprint, with our certificate) is tried again with a
//! growing wait until [`reconnect::RECONNECT_BUDGET`] is spent. What the
//! host or the user decided (goodbye, sharing stopped, a refusal, the
//! disconnect button) ends the session at once and for good.
//!
//! The quality the user picked is asked for again on the new connection
//! (the host forgets it with the old one); the monitor needs nothing, the
//! host keeps it. When the new connection streams another size, the window
//! hears of it as of any stream change.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bytes::Bytes;
use net::{ControlReceiver, ControlSender, Fingerprint, Identity, VideoChannel};
use protocol::control::{ControlMessage, StreamInfo};
use protocol::session::Quality;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;
use winit::event_loop::EventLoopProxy;

use super::clipboard::ViewerClipboard;
use super::failure::ViewerFailure;
use super::feed::{self, Restart};
use super::live_handle::LiveHandle;
use super::rate::TargetRate;
use super::reconnect::{self, Backoff, RECONNECT_BUDGET};
use super::window::UserEvent;
use super::{Connected, connect, outbox};

/// Time the render thread gets to start its pipeline over; past it the
/// session goes on anyway (a render thread that is gone ends the window).
const RESTART_WAIT: Duration = Duration::from_secs(2);

/// The two halves of the control channel of one connection.
pub struct ControlLink {
    pub tx: ControlSender,
    pub rx: ControlReceiver,
}

/// What it takes to connect to the same host again.
pub struct Reconnection {
    pub addr: SocketAddr,
    /// The certificate the host proved the first time, pinned from then on
    /// (also for a host never remembered, so another PC cannot take its
    /// place in the middle of a session).
    pub host: Fingerprint,
    /// The stream followed now (as the first connection streamed it, or as
    /// a later Welcome or `StreamChanged` said).
    pub stream: StreamInfo,
    pub identity: Option<Identity>,
    /// The PIN that came from `--pin` or the environment, in memory only:
    /// used by the first successful reconnection and forgotten after it (by
    /// then the host knows our certificate). Never written anywhere.
    pub pin: Option<String>,
}

/// How the video of the current connection reaches the render thread.
pub struct VideoFeed {
    pub forward: JoinHandle<()>,
    pub datagrams: std::sync::mpsc::SyncSender<Bytes>,
    pub restart: Restart,
    pub handle: LiveHandle,
}

impl VideoFeed {
    /// Moves the video to `video`, the channel of the new connection: stops
    /// the old forwarding, has the render thread start over (so nothing of
    /// the old stream remains, and a keyframe is asked for) and only then
    /// forwards the new datagrams.
    async fn switch_to(&mut self, video: VideoChannel, handle: net::SessionHandle) {
        self.forward.abort();
        let _ = tokio::time::timeout(RESTART_WAIT, &mut self.forward).await;
        self.restart.request();
        if !self.restart.wait_done(RESTART_WAIT).await {
            eprintln!("warning: the video pipeline did not restart in time");
        }
        self.handle.replace(handle);
        self.forward = tokio::spawn(feed::forward(video, self.datagrams.clone()));
    }
}

/// Everything the control task works with.
pub struct ControlTask {
    pub link: ControlLink,
    pub outgoing: UnboundedReceiver<ControlMessage>,
    pub window: EventLoopProxy<UserEvent>,
    pub target: TargetRate,
    pub clipboard: ViewerClipboard,
    pub reconnection: Reconnection,
    pub feed: VideoFeed,
}

/// How a connection's turn ended.
enum Served {
    /// We said goodbye (or nobody is left to send requests).
    Finished,
    Ended(ViewerFailure),
}

/// Why no new connection came out of [`reestablish`].
#[derive(Debug, PartialEq, Eq)]
enum Stop {
    /// The user disconnected while the viewer was reconnecting.
    Cancelled,
    Failed(ViewerFailure),
}

/// Runs until the session is over, reconnecting when the connection breaks
/// by itself. A session that ends for any other reason tells the window why
/// ([`UserEvent::HostLeft`]).
pub async fn run(task: ControlTask) {
    let ControlTask {
        mut link,
        mut outgoing,
        window,
        target,
        mut clipboard,
        mut reconnection,
        mut feed,
    } = task;
    let mut quality = Quality::Automatic;
    let reason = loop {
        let turn = Turn {
            outgoing: &mut outgoing,
            window: &window,
            target: &target,
            clipboard: &mut clipboard,
            quality: &mut quality,
            stream: &mut reconnection.stream,
        };
        let failure = match serve(&mut link, turn).await {
            Served::Finished => return,
            Served::Ended(failure) => failure,
        };
        if !reconnect::is_retryable(&failure) {
            break failure;
        }
        let _ = window.send_event(UserEvent::Reconnecting);
        println!("{failure}; reconnecting...");
        feed.handle.close("connection lost");
        let resumed = match reestablish(failure, &mut outgoing, &reconnection).await {
            Ok(connected) => resume(connected, &mut reconnection, &mut feed).await,
            Err(stop) => Err(stop),
        };
        match resumed {
            Ok(Resumed {
                link: new_link,
                input_allowed,
                stream,
            }) => {
                link = new_link;
                clipboard.set_host_allows(input_allowed);
                let _ = window.send_event(UserEvent::InputAllowed(input_allowed));
                if let Some(stream) = stream {
                    let _ = window.send_event(UserEvent::StreamChanged(stream));
                }
                if let Some(request) = quality_request(quality) {
                    // A link already broken again shows in the next `serve`.
                    let _ = link.tx.send(&request).await;
                }
                let _ = window.send_event(UserEvent::Reconnected);
                println!("Reconnected.");
            }
            Err(Stop::Cancelled) => return,
            Err(Stop::Failed(failure)) => break failure,
        }
    };
    let _ = window.send_event(UserEvent::HostLeft(reason));
}

/// What a connection's turn works with, besides its link.
struct Turn<'a> {
    outgoing: &'a mut UnboundedReceiver<ControlMessage>,
    window: &'a EventLoopProxy<UserEvent>,
    target: &'a TargetRate,
    clipboard: &'a mut ViewerClipboard,
    /// The quality last asked for, to ask for it again after a break.
    quality: &'a mut Quality,
    /// The stream followed, kept up to date with `StreamChanged`.
    stream: &'a mut StreamInfo,
}

/// Serves one connection: forwards our requests, applies what the host
/// sends. The host's target bitrate goes to `target`, for the statistics.
/// Copied text travels both ways through `clipboard`; `quality` and
/// `stream` follow what the user asks and the host streams.
async fn serve(link: &mut ControlLink, turn: Turn<'_>) -> Served {
    let ControlLink { tx, rx } = link;
    let Turn {
        outgoing,
        window,
        target,
        clipboard,
        quality,
        stream,
    } = turn;
    loop {
        tokio::select! {
            message = outgoing.recv() => {
                let Some(first) = message else { return Served::Finished };
                for message in outbox::next_batch(first, outgoing) {
                    if let Err(e) = tx.send(&message).await {
                        return Served::Ended(ViewerFailure::ConnectionLost(format!("{e:#}")));
                    }
                    match message {
                        ControlMessage::Bye => return Served::Finished,
                        ControlMessage::SetQuality(asked) => *quality = asked,
                        _ => {}
                    }
                }
            }
            () = clipboard.changed() => {
                if let Some(message) = clipboard.local_change().await
                    && let Err(e) = tx.send(&message).await
                {
                    return Served::Ended(ViewerFailure::ConnectionLost(format!("{e:#}")));
                }
            }
            incoming = rx.recv() => match incoming {
                Ok(Some(ControlMessage::Bye)) => {
                    return Served::Ended(ViewerFailure::HostDisconnected);
                }
                Ok(None) => return Served::Ended(ViewerFailure::HostLeft),
                Ok(Some(ControlMessage::InputAllowed(allowed))) => {
                    clipboard.set_host_allows(allowed);
                    let _ = window.send_event(UserEvent::InputAllowed(allowed));
                }
                Ok(Some(ControlMessage::TargetBitrate { bps })) => target.set(bps),
                Ok(Some(ControlMessage::Clipboard(text))) => {
                    clipboard.remote_text(&text).await;
                }
                Ok(Some(message)) => {
                    if let ControlMessage::StreamChanged(changed) = &message {
                        *stream = changed.clone();
                    }
                    if let Some(event) = window_event(message) {
                        let _ = window.send_event(event);
                    }
                }
                Err(e) => {
                    return Served::Ended(ViewerFailure::ConnectionLost(format!("{e:#}")));
                }
            }
        }
    }
}

/// What the window does with a message of the host, if it is one of
/// those: the pointer, the monitors, the stream starting over.
fn window_event(message: ControlMessage) -> Option<UserEvent> {
    Some(match message {
        ControlMessage::Cursor(shape) => UserEvent::Cursor(shape),
        ControlMessage::CursorVisible(visible) => UserEvent::CursorVisible(visible),
        ControlMessage::MonitorList { monitors, current } => {
            UserEvent::Monitors { monitors, current }
        }
        ControlMessage::StreamChanged(stream) => UserEvent::StreamChanged(stream),
        _ => return None,
    })
}

/// What a new connection is asked first: the quality the user picked, as
/// a new connection starts at the host's own.
fn quality_request(quality: Quality) -> Option<ControlMessage> {
    (quality != Quality::Automatic).then_some(ControlMessage::SetQuality(quality))
}

/// Connects to the host again, trying after 0, 0.5, 1, 2, 4... seconds (see
/// [`Backoff`]) until a connection is welcomed, the host gives an answer
/// that retrying cannot change, the budget is spent, or the user
/// disconnects. `cause` is why the connection broke.
async fn reestablish(
    cause: ViewerFailure,
    outgoing: &mut UnboundedReceiver<ControlMessage>,
    plan: &Reconnection,
) -> Result<Connected, Stop> {
    let mut backoff = Backoff::new(Instant::now());
    let mut last = cause;
    loop {
        let Some(delay) = backoff.next_delay(Instant::now()) else {
            return Err(Stop::Failed(gave_up(&last)));
        };
        let attempt = async {
            tokio::time::sleep(delay).await;
            connect(
                plan.addr,
                Some(plan.host),
                plan.identity.as_ref(),
                plan.pin.clone(),
                || {},
            )
            .await
        };
        let limit = backoff.remaining(Instant::now());
        let outcome = tokio::select! {
            () = until_cancelled(outgoing) => return Err(Stop::Cancelled),
            outcome = tokio::time::timeout(limit, attempt) => outcome,
        };
        match outcome {
            Ok(Ok(connected)) => return Ok(connected),
            Ok(Err(error)) if !reconnect::is_retryable_error(&error) => {
                return Err(Stop::Failed(failure_of(error)));
            }
            Ok(Err(error)) => last = failure_of(error),
            Err(_elapsed) => return Err(Stop::Failed(gave_up(&last))),
        }
    }
}

/// A welcomed connection put to work.
struct Resumed {
    link: ControlLink,
    /// Whether the host allows remote control now.
    input_allowed: bool,
    /// The stream, when it is not the one followed before the break (e.g.
    /// another resolution).
    stream: Option<StreamInfo>,
}

/// Puts a welcomed connection to work: checks its stream can be decoded
/// as the old one (same codec), has the render thread start over on it and
/// returns what changed.
async fn resume(
    connected: Connected,
    plan: &mut Reconnection,
    feed: &mut VideoFeed,
) -> Result<Resumed, Stop> {
    let Connected {
        stream,
        input_allowed,
        tx,
        rx,
        video,
        handle,
        ..
    } = connected;
    if stream.codec != plan.stream.codec {
        handle.close("video format changed");
        return Err(Stop::Failed(ViewerFailure::ConnectionLost(
            "the host changed its video format".to_string(),
        )));
    }
    feed.switch_to(video, handle).await;
    plan.pin = None;
    let changed = (stream != plan.stream).then(|| {
        plan.stream = stream.clone();
        stream
    });
    Ok(Resumed {
        link: ControlLink { tx, rx },
        input_allowed,
        stream: changed,
    })
}

/// Resolves when the user ends the session (goodbye, or the window is gone).
/// Whatever else is sent meanwhile is dropped: a click or a key from before
/// the connection came back must not reach the host after it.
async fn until_cancelled(outgoing: &mut UnboundedReceiver<ControlMessage>) {
    loop {
        match outgoing.recv().await {
            Some(ControlMessage::Bye) | None => return,
            Some(_) => {}
        }
    }
}

/// The failure an attempt's error stands for; an error that is not a known
/// failure (a local problem) counts as the connection being lost.
fn failure_of(error: anyhow::Error) -> ViewerFailure {
    error
        .downcast::<ViewerFailure>()
        .unwrap_or_else(|error| ViewerFailure::ConnectionLost(format!("{error:#}")))
}

/// What the viewer ends with when every attempt failed: a lost connection.
fn gave_up(last: &ViewerFailure) -> ViewerFailure {
    let last = match last {
        ViewerFailure::ConnectionLost(text) => text.clone(),
        other => other.to_string(),
    };
    ViewerFailure::ConnectionLost(format!(
        "no connection again within {} s ({last})",
        RECONNECT_BUDGET.as_secs()
    ))
}

#[cfg(test)]
mod tests;
