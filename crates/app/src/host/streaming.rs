//! The capture/encode thread of one session.
//!
//! GPU objects (capture, encoder) are not `Send`-friendly and the pipeline
//! is a blocking loop, so each session gets a dedicated OS thread running
//! `HostStreamer`, steered from async code through atomics and channels.
//! The thread also fits the encoder's bitrate to the network (see
//! `rate_adaptation`), helped by the viewer's reports the session hands in.
//! The frames it sends are kept for a moment in [`RecentFrames`], from which
//! the session resends at once the datagrams the viewer asks for, without
//! waiting for the thread's next frame.
//!
//! The stream starts over on the same connection, its frames numbered on,
//! when the viewer picks another monitor or quality
//! ([`StreamingThread::reconfigure`]) and when the monitor changes
//! resolution: capture and encoder are opened again (see `stream_setup`)
//! and the session hears it as [`StreamUpdate::Restarted`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use codec::{DesktopSource, HwEncoder, MediaFoundation};
use net::VideoChannel;
use pipeline::{HostStats, HostStreamer, RecentFrames, StreamContinuation};
use protocol::control::{ControlMessage, StreamInfo};
use protocol::feedback::ReceptionReport;
use protocol::recovery::Nack;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use super::events::EventSink;
use super::rate_adaptation::{Counters, RateAdaptation, RateChange, ViewerReports};
use super::screen_choice::ScreenChoice;
use super::stream_setup::{OpenStream, is_mode_change, open_stream, reopen_stream};
use super::{HostEvent, HostWarning};
use crate::options::StreamOptions;
use crate::realtime::{MmcssRegistration, TimerResolution};
use crate::wire::pointer_messages;

type Streamer = HostStreamer<DesktopSource, HwEncoder, VideoChannel>;

/// What the streaming thread has for the session.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum StreamUpdate {
    /// For the viewer as it is: pointer shape and visibility, the target
    /// bitrate.
    Viewer(ControlMessage),
    /// Capture and encoder started over for `choice` (the one asked for,
    /// or the previous one when that could not start); the video is `info`
    /// from now on.
    Restarted {
        choice: ScreenChoice,
        info: StreamInfo,
    },
}

/// What the thread streams: the host's settings and the viewer's choice
/// applied to them (see [`ScreenChoice::options`]).
pub(super) struct StreamPlan {
    pub(super) base: StreamOptions,
    pub(super) choice: ScreenChoice,
}

/// What the streaming thread did; `error` is why it stopped early, if so.
pub(super) struct StreamEnd {
    pub(super) stats: HostStats,
    pub(super) error: Option<anyhow::Error>,
}

/// Owns the capture/encode thread of one session.
pub(super) struct StreamingThread {
    stop: Arc<AtomicBool>,
    keyframe: Arc<AtomicBool>,
    reports: Arc<ViewerReports>,
    recent: RecentFrames,
    /// Where resent datagrams go: the connection the thread streams on.
    video: VideoChannel,
    /// The choices to stream, newest last.
    choices: mpsc::Sender<ScreenChoice>,
    done: Arc<tokio::sync::Notify>,
    ready: Option<oneshot::Receiver<Result<StreamInfo>>>,
    go: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<Result<StreamEnd>>>,
}

impl StreamingThread {
    /// `updates` receives what the stream produces for the session (see
    /// [`StreamUpdate`]). `events` gets the warnings of an encoder that
    /// cannot adapt its rate and of a choice that could not be streamed.
    pub(super) fn start(
        plan: StreamPlan,
        video: VideoChannel,
        updates: UnboundedSender<StreamUpdate>,
        events: EventSink,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let keyframe = Arc::new(AtomicBool::new(false));
        let reports = Arc::new(ViewerReports::default());
        let recent = RecentFrames::new();
        let done = Arc::new(tokio::sync::Notify::new());
        let (ready_tx, ready_rx) = oneshot::channel();
        let (go_tx, go_rx) = mpsc::channel();
        let (choices_tx, choices_rx) = mpsc::channel();
        let thread = {
            let (stop, keyframe, reports, recent, video, done) = (
                stop.clone(),
                keyframe.clone(),
                reports.clone(),
                recent.clone(),
                video.clone(),
                done.clone(),
            );
            std::thread::Builder::new()
                .name("wshare-stream".into())
                .spawn(move || {
                    let control = StreamControl {
                        stop: &stop,
                        keyframe: &keyframe,
                        reports,
                        recent,
                        choices: choices_rx,
                        updates,
                        events,
                    };
                    let result = stream_loop(&plan, video, control, ready_tx, go_rx);
                    done.notify_one();
                    result
                })
                .context("spawning the streaming thread")?
        };
        Ok(Self {
            stop,
            keyframe,
            reports,
            recent,
            video,
            choices: choices_tx,
            done,
            ready: Some(ready_rx),
            go: Some(go_tx),
            thread: Some(thread),
        })
    }

    /// Waits until capture and encoder are set up.
    pub(super) async fn ready(&mut self) -> Result<StreamInfo> {
        let receiver = self.ready.take().context("ready() awaited twice")?;
        receiver
            .await
            .map_err(|_| anyhow!("streaming thread exited"))?
    }

    /// Starts sending frames (after the viewer got Welcome).
    pub(super) fn go(&self) {
        if let Some(go) = &self.go {
            let _ = go.send(());
        }
    }

    pub(super) fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }

    /// Starts the stream over for `choice`, soon, without blocking. Choices
    /// made in a quick row are merged: only the newest is streamed. The
    /// outcome arrives as [`StreamUpdate::Restarted`].
    pub(super) fn reconfigure(&self, choice: ScreenChoice) {
        // Fails only once the thread is gone, which `finished` reports.
        let _ = self.choices.send(choice);
    }

    /// Hands a viewer's reception report to the bitrate adaptation.
    pub(super) fn record_report(&self, report: &ReceptionReport) {
        self.reports.record(report);
    }

    /// Sends again the datagrams `nack` asks for, those still kept. Errors
    /// only when the video connection can no longer send.
    pub(super) fn resend(&self, nack: &Nack) -> Result<()> {
        self.recent
            .resend(nack, &self.video, Instant::now())
            .context("resending video datagrams")?;
        Ok(())
    }

    /// Resolves when the thread has stopped on its own (error or viewer
    /// gone).
    pub(super) async fn finished(&self) {
        self.done.notified().await;
    }

    /// Stops the thread and waits for it, without blocking the runtime.
    pub(super) async fn stop(mut self) -> Result<StreamEnd> {
        self.signal_stop();
        let thread = self.thread.take().expect("joined once");
        tokio::task::spawn_blocking(move || thread.join())
            .await?
            .map_err(|_| anyhow!("streaming thread panicked"))?
    }

    fn signal_stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.go = None; // unblocks a thread still waiting for `go`
    }
}

impl Drop for StreamingThread {
    /// A session that bails out early (`?`) must not leave the thread
    /// capturing forever; it is told to stop and detached.
    fn drop(&mut self) {
        self.signal_stop();
    }
}

/// How the session talks to its streaming thread.
struct StreamControl<'a> {
    stop: &'a AtomicBool,
    keyframe: &'a AtomicBool,
    reports: Arc<ViewerReports>,
    /// Shared with [`StreamingThread::resend`].
    recent: RecentFrames,
    choices: mpsc::Receiver<ScreenChoice>,
    updates: UnboundedSender<StreamUpdate>,
    events: EventSink,
}

impl StreamControl<'_> {
    /// Queues `update` for the session. Fails only once the session is
    /// over, when nothing is left to tell.
    fn tell_session(&self, update: StreamUpdate) {
        let _ = self.updates.send(update);
    }

    fn tell_viewer(&self, message: ControlMessage) {
        self.tell_session(StreamUpdate::Viewer(message));
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// The newest choice the session sent since the last look, if any.
    fn newest_choice(&self) -> Option<ScreenChoice> {
        self.choices.try_iter().last()
    }
}

/// The stream being sent, its bitrate adaptation and what it streams.
struct Live {
    streamer: Streamer,
    rate: RateAdaptation,
    choice: ScreenChoice,
}

impl Live {
    /// Starts adapting the bitrate of `streamer`, whose ceiling is
    /// `max_bps`, and tells the viewer the rate it starts at.
    fn new(
        streamer: Streamer,
        choice: ScreenChoice,
        max_bps: u32,
        transport: &VideoChannel,
        control: &StreamControl<'_>,
    ) -> Self {
        let rate = RateAdaptation::new(
            max_bps,
            control.reports.clone(),
            counters(transport, streamer.stats().frames_skipped),
            Instant::now(),
        );
        control.tell_viewer(ControlMessage::TargetBitrate { bps: max_bps });
        Self {
            streamer,
            rate,
            choice,
        }
    }
}

/// Body of the streaming thread: set up GPU objects, report readiness, then
/// run the host pipeline until told to stop, starting the stream over when
/// asked or when the monitor changes mode.
fn stream_loop(
    plan: &StreamPlan,
    video: VideoChannel,
    control: StreamControl<'_>,
    ready: oneshot::Sender<Result<StreamInfo>>,
    go: mpsc::Receiver<()>,
) -> Result<StreamEnd> {
    let options = plan.choice.options(&plan.base);
    let setup = MediaFoundation::start().and_then(|mf| Ok((mf, open_stream(&options)?)));
    let (_mf, opened) = match setup {
        Ok(parts) => parts,
        Err(e) => {
            let message = format!("{e:#}");
            let _ = ready.send(Err(e));
            return Err(anyhow!(message));
        }
    };
    let OpenStream {
        source,
        encoder,
        info,
    } = opened;
    let _ = ready.send(Ok(info));
    if go.recv().is_err() {
        return Ok(StreamEnd {
            stats: HostStats::default(),
            error: None,
        });
    }

    // Held for the whole streaming loop, released when it ends.
    let _timer = TimerResolution::raise();
    let _mmcss = MmcssRegistration::register();
    let transport = video.clone();
    let max_bps = options.bitrate_bps();
    let streamer = HostStreamer::new(
        source,
        encoder,
        video,
        options.fps,
        max_bps,
        control.recent.clone(),
    );
    let mut live = Live::new(streamer, plan.choice, max_bps, &transport, &control);
    while !control.stopped() {
        if let Some(wanted) = control
            .newest_choice()
            .filter(|wanted| *wanted != live.choice)
        {
            live = match restart(live, wanted, plan, &transport, &control) {
                Ok(live) => live,
                Err(end) => return Ok(end),
            };
        }
        let step = (|| {
            if control.keyframe.swap(false, Ordering::Relaxed) {
                live.streamer.request_keyframe()?;
            }
            live.streamer.step()
        })();
        match step {
            Ok(()) => {}
            Err(e) if is_mode_change(&e) => {
                let same = live.choice;
                live = match restart(live, same, plan, &transport, &control) {
                    Ok(live) => live,
                    Err(end) => return Ok(end),
                };
                continue;
            }
            Err(e) => {
                return Ok(StreamEnd {
                    stats: live.streamer.stats().clone(),
                    error: Some(e),
                });
            }
        }
        // Pointer changes are collected while capturing; forward them.
        for message in pointer_messages(live.streamer.source_mut().take_pointer_changes()) {
            control.tell_viewer(message);
        }
        adapt_bitrate(&mut live, &transport, &control);
    }
    Ok(StreamEnd {
        stats: live.streamer.stats().clone(),
        error: None,
    })
}

/// Closes the capture and encoder of `live` and opens them for `wanted`,
/// the stream going on with the next frame id. If `wanted` cannot start,
/// the previous choice is opened again (and the failure reported); if that
/// fails too, the stream ends with the returned [`StreamEnd`].
fn restart(
    live: Live,
    wanted: ScreenChoice,
    plan: &StreamPlan,
    transport: &VideoChannel,
    control: &StreamControl<'_>,
) -> Result<Live, StreamEnd> {
    let previous = live.choice;
    // Closed first: the same monitor cannot be duplicated twice at once.
    let continuation = live.streamer.into_continuation();
    let reopen =
        |choice: ScreenChoice| reopen_stream(&choice.options(&plan.base), || control.stopped());
    let (opened, choice) = match reopen(wanted) {
        Ok(opened) => (opened, wanted),
        Err(e) if wanted != previous => {
            let warning = HostWarning::ScreenUnavailable(format!("{e:#}"));
            control.events.emit(HostEvent::Warning(warning));
            match reopen(previous) {
                Ok(opened) => (opened, previous),
                Err(e) => return Err(ended(&continuation, e, control)),
            }
        }
        Err(e) => return Err(ended(&continuation, e, control)),
    };
    let OpenStream {
        source,
        encoder,
        info,
    } = opened;
    control.tell_session(StreamUpdate::Restarted { choice, info });
    let options = choice.options(&plan.base);
    let max_bps = options.bitrate_bps();
    let streamer = HostStreamer::resume(continuation, source, encoder, options.fps, max_bps);
    Ok(Live::new(streamer, choice, max_bps, transport, control))
}

/// How a stream that could not start over ends: with `error`, unless the
/// session was stopping anyway.
fn ended(
    continuation: &StreamContinuation<VideoChannel>,
    error: anyhow::Error,
    control: &StreamControl<'_>,
) -> StreamEnd {
    StreamEnd {
        stats: continuation.stats().clone(),
        error: (!control.stopped()).then_some(error),
    }
}

/// Lets the encoder follow the network when a sample is due, and tells the
/// viewer its new target. An encoder that refuses a rate is reported once;
/// the stream then stays at the rate it has.
fn adapt_bitrate(live: &mut Live, transport: &VideoChannel, control: &StreamControl<'_>) {
    let Live { streamer, rate, .. } = live;
    let frames_skipped = streamer.stats().frames_skipped;
    let change = rate.poll(
        Instant::now(),
        || counters(transport, frames_skipped),
        |bps| streamer.set_bitrate(bps),
    );
    match change {
        None => {}
        Some(RateChange::Retargeted(bps)) => {
            control.tell_viewer(ControlMessage::TargetBitrate { bps });
        }
        Some(RateChange::Fixed(e)) => {
            let warning = HostWarning::BitrateFixed(format!("{e:#}"));
            control.events.emit(HostEvent::Warning(warning));
        }
    }
}

fn counters(transport: &VideoChannel, frames_skipped: u64) -> Counters {
    Counters {
        transport: transport.transport_stats(),
        frames_skipped,
    }
}
