//! `wshare host`: announce on the LAN and stream the screen to one viewer
//! at a time.
//!
//! Threads: the tokio runtime handles networking (accept, control channel);
//! each session gets a dedicated blocking thread that owns the GPU objects
//! (capture, encoder) and runs `HostStreamer`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use codec::{DesktopSource, HwEncoder, MediaFoundation};
use discovery::{Advertiser, HostAnnouncement, computer_name};
use net::{ControlReceiver, ControlSender, HostEndpoint, Identity, Session, VideoChannel};
use pipeline::{HostStats, HostStreamer};
use protocol::control::{ControlMessage, PROTOCOL_VERSION, StreamInfo};
use input::{Outcome, RemoteInput, ScreenArea};
use protocol::input::InputEvent;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

use crate::cli::{HostOptions, StreamOptions};
use crate::handshake::{HelloVerdict, check_hello, generate_pin};
use crate::paths;
use crate::wire::{pointer_messages, to_wire};

/// How long a connected viewer has to send Hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Delay after a wrong PIN, to make guessing slow.
const REJECT_DELAY: Duration = Duration::from_secs(1);

pub fn run(options: HostOptions) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve(options))
}

async fn serve(options: HostOptions) -> Result<()> {
    let name = computer_name();
    let identity = Identity::load_or_create(&paths::config_dir()?, &name)?;
    let endpoint = HostEndpoint::bind_any(options.port, &identity)?;
    let port = endpoint.local_addr()?.port();
    let pin = options.pin.clone().unwrap_or_else(generate_pin);

    let monitor = capture::list_monitors()?
        .into_iter()
        .nth(options.stream.monitor)
        .with_context(|| format!("monitor {} not found", options.stream.monitor))?;
    let fingerprint = identity.fingerprint().to_string();
    let announcement = HostAnnouncement {
        id: fingerprint[..16].to_string(),
        display_name: name.clone(),
        port,
        protocol_version: PROTOCOL_VERSION,
        width: monitor.width,
        height: monitor.height,
    };
    // Discovery is a convenience: viewers can still connect by IP.
    let _advertiser = Advertiser::start(&announcement)
        .inspect_err(|e| eprintln!("warning: LAN announcement failed: {e:#}"))
        .ok();

    println!("Sharing {} ({}x{}) as \"{name}\" on UDP port {port}", monitor.name, monitor.width, monitor.height);
    println!("PIN: {pin}");
    println!("Host fingerprint: {fingerprint}");
    println!("Waiting for a viewer... (Ctrl+C to stop)");

    loop {
        let session = endpoint.accept().await?;
        let remote = session.remote_addr();
        match run_session(session, &options, &pin, &name).await {
            Ok(Some(stats)) => println!(
                "{remote} disconnected: {} frames sent ({} keyframes), {:.1} MB",
                stats.frames_sent,
                stats.keyframes_sent,
                stats.bytes_sent as f64 / 1e6
            ),
            Ok(None) => {}
            Err(e) => eprintln!("{remote}: session ended with error: {e:#}"),
        }
    }
}

/// Handles one viewer from Hello to disconnect. `None` if it was rejected.
async fn run_session(
    session: Session,
    options: &HostOptions,
    pin: &str,
    host_name: &str,
) -> Result<Option<HostStats>> {
    let stream = &options.stream;
    let remote = session.remote_addr();
    let handle = session.handle();
    let (mut tx, mut rx, video) = session.split();

    let hello = tokio::time::timeout(HELLO_TIMEOUT, rx.recv())
        .await
        .context("viewer sent no Hello")??
        .context("viewer left before Hello")?;
    let viewer_name = match check_hello(&hello, pin) {
        HelloVerdict::Accept { viewer_name } => viewer_name,
        HelloVerdict::Reject { reason } => {
            println!("{remote}: rejected ({reason})");
            tokio::time::sleep(REJECT_DELAY).await;
            let _ = tx.send(&ControlMessage::Rejected { reason }).await;
            let _ = tx.finish();
            handle.closed().await;
            return Ok(None);
        }
    };

    // Created before streaming starts so a missing monitor fails early.
    let mut input = if options.view_only { None } else { Some(remote_input(stream.monitor)?) };
    let (cursor_tx, mut cursor_rx) = unbounded_channel();
    let mut streaming = StreamingThread::start(stream.clone(), video, cursor_tx);
    let info = match streaming.ready().await {
        Ok(info) => info,
        Err(e) => {
            let reason = format!("host cannot stream: {e:#}");
            let _ = tx.send(&ControlMessage::Rejected { reason }).await;
            return Err(e);
        }
    };
    tx.send(&ControlMessage::Welcome {
        host_name: host_name.to_string(),
        stream: info.clone(),
        input_allowed: !options.view_only,
    })
    .await?;
    println!(
        "{remote}: \"{viewer_name}\" connected - streaming {}x{} @ {} fps {:?}",
        info.width, info.height, info.fps, info.codec
    );
    streaming.go();

    let channels = Channels { rx: &mut rx, tx: &mut tx, cursor: &mut cursor_rx };
    let ended = serve_control(channels, &streaming, input.as_mut()).await;
    drop(input); // releases any key or button the viewer still held
    handle.close("session over");
    let StreamEnd { stats, error } = streaming.stop().await?;
    match (ended?, error) {
        // Sends fail once the viewer has gone: that is a normal end.
        (SessionEnd::ViewerLeft, _) | (SessionEnd::StreamStopped, None) => Ok(Some(stats)),
        (SessionEnd::StreamStopped, Some(e)) => Err(e),
    }
}

/// Who ended the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEnd {
    ViewerLeft,
    StreamStopped,
}

/// Grace period to tell "viewer left" from "our stream failed" when the
/// streaming thread stops first.
const LEAVE_GRACE: Duration = Duration::from_millis(300);

/// What the streaming thread did; `error` is why it stopped early, if so.
struct StreamEnd {
    stats: HostStats,
    error: Option<anyhow::Error>,
}

/// Injection into the shared monitor's area of the virtual desktop.
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

/// Injects one remote event. Failures are reported but never end the
/// session: a blocked or odd event must not cut the viewer off.
fn apply_input(input: Option<&mut RemoteInput>, event: &InputEvent) {
    let Some(input) = input else {
        return; // view-only host
    };
    match input.handle(event) {
        Ok(Outcome::Blocked { first: true }) => println!(
            "Remote input blocked by Windows (elevated window, UAC or lock screen); \
             it resumes when a normal window has focus"
        ),
        Ok(_) => {}
        Err(e) => eprintln!("Remote input failed: {e:#}"),
    }
}

/// The session's message channels: control stream both ways, plus pointer
/// updates produced by the streaming thread.
struct Channels<'a> {
    rx: &'a mut ControlReceiver,
    tx: &'a mut ControlSender,
    cursor: &'a mut UnboundedReceiver<ControlMessage>,
}

/// Serves the viewer until either side stops: forwards its requests and
/// input, and sends it our pointer updates.
async fn serve_control(
    channels: Channels<'_>,
    streaming: &StreamingThread,
    mut input: Option<&mut RemoteInput>,
) -> Result<SessionEnd> {
    let Channels { rx, tx, cursor } = channels;
    loop {
        tokio::select! {
            message = rx.recv() => match message? {
                Some(ControlMessage::RequestKeyframe) => streaming.request_keyframe(),
                Some(ControlMessage::Input(event)) => apply_input(input.as_deref_mut(), &event),
                Some(ControlMessage::Bye) | None => return Ok(SessionEnd::ViewerLeft),
                Some(_) => {} // ignore unexpected messages
            },
            Some(update) = cursor.recv() => tx.send(&update).await?,
            () = streaming.finished() => {
                // The thread usually stops because the viewer just closed the
                // connection; its Bye (or the close) is then already here.
                return Ok(match tokio::time::timeout(LEAVE_GRACE, rx.recv()).await {
                    Ok(Ok(Some(ControlMessage::Bye) | None) | Err(_)) => SessionEnd::ViewerLeft,
                    _ => {
                        let _ = tx.send(&ControlMessage::Bye).await;
                        SessionEnd::StreamStopped
                    }
                });
            }
        }
    }
}

/// Owns the capture/encode thread of one session.
struct StreamingThread {
    stop: Arc<AtomicBool>,
    keyframe: Arc<AtomicBool>,
    done: Arc<tokio::sync::Notify>,
    ready: Option<oneshot::Receiver<Result<StreamInfo>>>,
    go: mpsc::Sender<()>,
    thread: Option<JoinHandle<Result<StreamEnd>>>,
}

impl StreamingThread {
    /// cursor receives pointer shape/visibility changes as control messages.
    fn start(
        stream: StreamOptions,
        video: VideoChannel,
        cursor: UnboundedSender<ControlMessage>,
    ) -> Self {
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
                    let control = StreamControl { stop: &stop, keyframe: &keyframe, cursor };
                    let result = stream_loop(&stream, video, control, ready_tx, go_rx);
                    done.notify_one();
                    result
                })
                .expect("spawning the streaming thread")
        };
        Self { stop, keyframe, done, ready: Some(ready_rx), go: go_tx, thread: Some(thread) }
    }

    /// Waits until capture and encoder are set up.
    async fn ready(&mut self) -> Result<StreamInfo> {
        let receiver = self.ready.take().context("ready() awaited twice")?;
        receiver.await.map_err(|_| anyhow!("streaming thread exited"))?
    }

    fn go(&self) {
        let _ = self.go.send(());
    }

    fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }

    async fn finished(&self) {
        self.done.notified().await;
    }

    async fn stop(mut self) -> Result<StreamEnd> {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.go); // unblocks a thread still waiting for `go`
        let thread = self.thread.take().expect("joined once");
        tokio::task::spawn_blocking(move || thread.join())
            .await?
            .map_err(|_| anyhow!("streaming thread panicked"))?
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
        println!("Encoding with {} on {}", encoder.name(), gpu.adapter_name);
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
        return Ok(StreamEnd { stats: HostStats::default(), error: None });
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
    Ok(StreamEnd { stats: streamer.stats().clone(), error })
}
