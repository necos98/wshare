//! The capture/encode thread of one session.
//!
//! GPU objects (capture, encoder) are not `Send`-friendly and the pipeline
//! is a blocking loop, so each session gets a dedicated OS thread running
//! `HostStreamer`, steered from async code through atomics and channels.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};
use codec::{DesktopSource, HwEncoder, MediaFoundation};
use net::VideoChannel;
use pipeline::{HostStats, HostStreamer};
use protocol::control::{ControlMessage, StreamInfo};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::cli::StreamOptions;
use crate::wire::{pointer_messages, to_wire};

/// What the streaming thread did; `error` is why it stopped early, if so.
pub(super) struct StreamEnd {
    pub(super) stats: HostStats,
    pub(super) error: Option<anyhow::Error>,
}

/// Owns the capture/encode thread of one session.
pub(super) struct StreamingThread {
    stop: Arc<AtomicBool>,
    keyframe: Arc<AtomicBool>,
    done: Arc<tokio::sync::Notify>,
    ready: Option<oneshot::Receiver<Result<StreamInfo>>>,
    go: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<Result<StreamEnd>>>,
}

impl StreamingThread {
    /// `cursor` receives pointer shape/visibility changes as control
    /// messages.
    pub(super) fn start(
        stream: StreamOptions,
        video: VideoChannel,
        cursor: UnboundedSender<ControlMessage>,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let keyframe = Arc::new(AtomicBool::new(false));
        let done = Arc::new(tokio::sync::Notify::new());
        let (ready_tx, ready_rx) = oneshot::channel();
        let (go_tx, go_rx) = mpsc::channel();
        let thread = {
            let (stop, keyframe, done) = (stop.clone(), keyframe.clone(), done.clone());
            std::thread::Builder::new()
                .name("wshare-stream".into())
                .spawn(move || {
                    let control = StreamControl {
                        stop: &stop,
                        keyframe: &keyframe,
                        cursor,
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
    cursor: UnboundedSender<ControlMessage>,
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

    let mut streamer = HostStreamer::new(source, encoder, video, stream.fps);
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
            let _ = control.cursor.send(message);
        }
    }
    Ok(StreamEnd {
        stats: streamer.stats().clone(),
        error,
    })
}
