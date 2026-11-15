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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use codec::{DesktopSource, HwEncoder, MediaFoundation};
use net::VideoChannel;
use pipeline::{HostStats, HostStreamer, RecentFrames};
use protocol::control::{ControlMessage, StreamInfo};
use protocol::feedback::ReceptionReport;
use protocol::recovery::Nack;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use super::events::EventSink;
use super::rate_adaptation::{Counters, RateAdaptation, RateChange, ViewerReports};
use super::{HostEvent, HostWarning};
use crate::options::StreamOptions;
use crate::realtime::{MmcssRegistration, TimerResolution};
use crate::wire::{pointer_messages, to_wire};

type Streamer = HostStreamer<DesktopSource, HwEncoder, VideoChannel>;

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
    done: Arc<tokio::sync::Notify>,
    ready: Option<oneshot::Receiver<Result<StreamInfo>>>,
    go: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<Result<StreamEnd>>>,
}

impl StreamingThread {
    /// `updates` receives the control messages the stream produces for the
    /// viewer: pointer shape/visibility changes and the target bitrate.
    /// `events` gets the warning of an encoder that cannot adapt its rate.
    pub(super) fn start(
        stream: StreamOptions,
        video: VideoChannel,
        updates: UnboundedSender<ControlMessage>,
        events: EventSink,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let keyframe = Arc::new(AtomicBool::new(false));
        let reports = Arc::new(ViewerReports::default());
        let recent = RecentFrames::new();
        let done = Arc::new(tokio::sync::Notify::new());
        let (ready_tx, ready_rx) = oneshot::channel();
        let (go_tx, go_rx) = mpsc::channel();
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
                        updates,
                        events,
                    };
                    let result = stream_loop(&stream, video, control, ready_tx, go_rx);
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
    updates: UnboundedSender<ControlMessage>,
    events: EventSink,
}

impl StreamControl<'_> {
    /// Queues `message` for the viewer. Fails only once the session is
    /// over, when nothing is left to tell.
    fn tell_viewer(&self, message: ControlMessage) {
        let _ = self.updates.send(message);
    }
}

/// Body of the streaming thread: set up GPU objects, report readiness, then
/// run the host pipeline until told to stop.
fn stream_loop(
    stream: &StreamOptions,
    video: VideoChannel,
    control: StreamControl<'_>,
    ready: oneshot::Sender<Result<StreamInfo>>,
    go: mpsc::Receiver<()>,
) -> Result<StreamEnd> {
    let setup = (|| {
        let mf = MediaFoundation::start()?;
        let source = DesktopSource::open(stream.monitor, stream.fps)?;
        let gpu = source.gpu().clone();
        let config = stream.encoder_config(source.width(), source.height());
        let encoder = HwEncoder::new(&gpu.device, gpu.adapter_luid, &config)?;
        let info = StreamInfo {
            codec: to_wire(stream.codec),
            width: config.width,
            height: config.height,
            fps: stream.fps,
            encoder: encoder.name().to_string(),
        };
        anyhow::Ok((mf, source, encoder, info))
    })();
    let (_mf, source, encoder, info) = match setup {
        Ok(parts) => parts,
        Err(e) => {
            let message = format!("{e:#}");
            let _ = ready.send(Err(e));
            return Err(anyhow!(message));
        }
    };
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
    let max_bps = stream.bitrate_bps();
    let mut streamer = HostStreamer::new(
        source,
        encoder,
        video,
        stream.fps,
        max_bps,
        control.recent.clone(),
    );
    let mut rate = RateAdaptation::new(
        max_bps,
        control.reports.clone(),
        counters(&transport, streamer.stats().frames_skipped),
        Instant::now(),
    );
    control.tell_viewer(ControlMessage::TargetBitrate { bps: max_bps });
    let mut error = None;
    while !control.stop.load(Ordering::Relaxed) {
        let step = (|| {
            if control.keyframe.swap(false, Ordering::Relaxed) {
                streamer.request_keyframe()?;
            }
            streamer.step()
        })();
        if let Err(e) = step {
            error = Some(e);
            break;
        }
        // Pointer changes are collected while capturing; forward them.
        for message in pointer_messages(streamer.source_mut().take_pointer_changes()) {
            control.tell_viewer(message);
        }
        adapt_bitrate(&mut rate, &mut streamer, &transport, &control);
    }
    Ok(StreamEnd {
        stats: streamer.stats().clone(),
        error,
    })
}

/// Lets the encoder follow the network when a sample is due, and tells the
/// viewer its new target. An encoder that refuses a rate is reported once;
/// the stream then stays at the rate it has.
fn adapt_bitrate(
    rate: &mut RateAdaptation,
    streamer: &mut Streamer,
    transport: &VideoChannel,
    control: &StreamControl<'_>,
) {
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
