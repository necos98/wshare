//! The thread that owns decoder and renderer for one viewing session.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use codec::{HwDecoder, MediaFoundation};
use gpu::Gpu;
use pipeline::{ViewerAction, ViewerStats, ViewerStreamer};
use protocol::control::{ControlMessage, StreamInfo};
use render::{Renderer, ToolbarView};
use windows::Win32::Foundation::HWND;

use super::Outgoing;
use super::feed::next_batch;
use super::notice::{Notice, NoticeBoard};
use super::sink::ViewSink;
use super::stall::StallWatch;
use super::stats::{FrameTimer, Labels, RateMeter, Totals, overlay_lines};
use crate::realtime::MmcssRegistration;

/// How often the loop wakes up without datagrams (keyframe retries, resize,
/// stop flag, overlay refresh).
const TICK: Duration = Duration::from_millis(50);
/// Wake-up period while the control bar is open, so hover and collapse
/// changes show within a frame or two. Otherwise the loop keeps [`TICK`].
const TOOLBAR_TICK: Duration = Duration::from_millis(16);
/// How often the statistics overlay is refreshed while visible.
const OVERLAY_REFRESH: Duration = Duration::from_millis(500);

/// Reads the current network round-trip time (kept abstract so the render
/// loop does not depend on the transport).
pub type RttProbe = Box<dyn Fn() -> Duration + Send>;

/// Everything the render thread needs, moved into it at start.
pub struct RenderInput {
    /// The window handle as an integer: `HWND` itself is not `Send`.
    pub hwnd: isize,
    pub stream: StreamInfo,
    pub datagrams: Receiver<Bytes>,
    pub outgoing: Outgoing,
    pub rtt: RttProbe,
}

pub struct RenderThread {
    stop: Arc<AtomicBool>,
    size: Arc<AtomicU64>,
    stats_visible: Arc<AtomicBool>,
    toolbar: Arc<ToolbarSlot>,
    notices: Sender<Notice>,
    thread: Option<JoinHandle<Result<ViewerStats>>>,
}

impl RenderThread {
    /// `on_exit` runs on the render thread when it ends by itself (error).
    pub fn start(
        input: RenderInput,
        width: u32,
        height: u32,
        on_exit: impl FnOnce(&Result<ViewerStats>) + Send + 'static,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let size = Arc::new(AtomicU64::new(pack(width, height)));
        let stats_visible = Arc::new(AtomicBool::new(false));
        let toolbar = Arc::new(ToolbarSlot::default());
        let (notices, incoming_notices) = channel();
        let thread = {
            let (stop, size, stats_visible, toolbar) = (
                stop.clone(),
                size.clone(),
                stats_visible.clone(),
                toolbar.clone(),
            );
            std::thread::Builder::new()
                .name("wshare-render".into())
                .spawn(move || {
                    let shared = Shared {
                        stop: &stop,
                        size: &size,
                        stats_visible: &stats_visible,
                        toolbar: &toolbar,
                        notices: &incoming_notices,
                    };
                    let result = render_loop(input, shared);
                    if !stop.load(Ordering::Relaxed) {
                        on_exit(&result);
                    }
                    result
                })
                .context("spawning the render thread")?
        };
        Ok(Self {
            stop,
            size,
            stats_visible,
            toolbar,
            notices,
            thread: Some(thread),
        })
    }

    /// Shows `notice` over the video for its time.
    pub fn show_notice(&self, notice: Notice) {
        // The loop is gone only when the window is closing.
        let _ = self.notices.send(notice);
    }

    /// Shows or hides the statistics overlay; returns the new state.
    pub fn toggle_stats(&self) -> bool {
        !self.stats_visible.fetch_xor(true, Ordering::Relaxed)
    }

    /// Sets the control bar drawn over the video (`None`: no bar).
    pub fn set_toolbar(&self, view: Option<ToolbarView>) {
        self.toolbar.set(view);
    }

    pub fn resize(&self, width: u32, height: u32) {
        self.size.store(pack(width, height), Ordering::Relaxed);
    }

    /// Stops and joins the thread; must run before the window is destroyed.
    pub fn stop(&mut self) -> Option<Result<ViewerStats>> {
        self.stop.store(true, Ordering::Relaxed);
        let thread = self.thread.take()?;
        Some(
            thread
                .join()
                .unwrap_or_else(|_| Err(anyhow!("render thread panicked"))),
        )
    }
}

impl Drop for RenderThread {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// State shared between the window thread and the render thread.
struct Shared<'a> {
    stop: &'a AtomicBool,
    size: &'a AtomicU64,
    stats_visible: &'a AtomicBool,
    toolbar: &'a ToolbarSlot,
    notices: &'a Receiver<Notice>,
}

fn render_loop(input: RenderInput, shared: Shared<'_>) -> Result<ViewerStats> {
    let Shared {
        stop,
        size,
        stats_visible,
        toolbar,
        notices,
    } = shared;
    let _mmcss = MmcssRegistration::register();
    let _mf = MediaFoundation::start()?;
    // Decoder and renderer must share one device so decoded textures can
    // be drawn directly.
    let hwnd = HWND(input.hwnd as *mut _);
    let gpu = Gpu::for_window(hwnd)?;
    let codec = crate::wire::from_wire(input.stream.codec);
    let decoder = HwDecoder::new(&gpu, codec, input.stream.width, input.stream.height)?;
    let mut applied = unpack(size.load(Ordering::Relaxed));
    let renderer = Renderer::new(&gpu, hwnd, applied.0, applied.1)?;
    println!("Decoding with {} on {}", decoder.name(), gpu.adapter_name);
    let mut overlay =
        StatsOverlay::new(input.stream.clone(), decoder.name().to_string(), input.rtt);
    let mut streamer = ViewerStreamer::new(decoder, ViewSink::new(renderer));
    let started = Instant::now();
    let mut stall = StallWatch::new(started);
    let mut board = NoticeBoard::default();
    board.show(Notice::Shortcuts, started);
    let mut batch = Vec::new();

    while !stop.load(Ordering::Relaxed) {
        let timeout = if toolbar.active() { TOOLBAR_TICK } else { TICK };
        let action = match next_batch(&input.datagrams, timeout, &mut batch) {
            Ok(()) => {
                let (received, shown) = (Instant::now(), streamer.stats().frames_presented);
                stall.on_datagram(received);
                let action = streamer.on_datagrams(batch.iter().cloned(), received)?;
                overlay.on_batch(
                    batch.iter().map(Bytes::len).sum(),
                    streamer.stats().frames_presented > shown,
                    received,
                );
                action
            }
            Err(RecvTimeoutError::Timeout) => streamer.on_tick(Instant::now()),
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let now = Instant::now();
        let silence = stall.check(now);
        if action == ViewerAction::RequestKeyframe || silence.probe {
            let _ = input.outgoing.send(ControlMessage::RequestKeyframe);
        }
        board.set_unstable(silence.unstable);
        while let Ok(notice) = notices.try_recv() {
            board.show(notice, now);
        }
        if let Some(lines) = board.take_changed(now) {
            streamer.sink_mut().set_notice(lines)?;
        }
        let wanted = unpack(size.load(Ordering::Relaxed));
        if wanted != applied {
            streamer.sink_mut().resize(wanted.0, wanted.1)?;
            applied = wanted;
        }
        if let Some(view) = toolbar.take_change() {
            streamer.sink_mut().set_toolbar(view)?;
        }
        overlay.update(
            stats_visible.load(Ordering::Relaxed),
            &mut streamer,
            Instant::now(),
        )?;
    }
    Ok(streamer.stats().clone())
}

/// Keeps the statistics overlay's numbers and redraws it while visible.
struct StatsOverlay {
    stream: StreamInfo,
    decoder: String,
    rtt: RttProbe,
    meter: RateMeter,
    frame_time: FrameTimer,
    bytes_received: u64,
    shown: bool,
    next_refresh: Instant,
}

impl StatsOverlay {
    fn new(stream: StreamInfo, decoder: String, rtt: RttProbe) -> Self {
        Self {
            stream,
            decoder,
            rtt,
            meter: RateMeter::default(),
            frame_time: FrameTimer::default(),
            bytes_received: 0,
            shown: false,
            next_refresh: Instant::now(),
        }
    }

    /// presented: the batch of `len` bytes put a new frame on screen; the
    /// time since started is then its decode + present time.
    fn on_batch(&mut self, len: usize, presented: bool, started: Instant) {
        self.bytes_received += len as u64;
        if presented {
            self.frame_time.record(started.elapsed());
        }
    }

    fn update<D>(
        &mut self,
        visible: bool,
        streamer: &mut ViewerStreamer<D, ViewSink>,
        now: Instant,
    ) -> Result<()>
    where
        D: pipeline::VideoDecoder<Frame = gpu::GpuFrame>,
    {
        let stats = streamer.stats().clone();
        let totals = Totals {
            bytes_received: self.bytes_received,
            frames_presented: stats.frames_presented,
        };
        if !visible {
            if self.shown {
                self.shown = false;
                streamer.sink_mut().set_overlay(None)?;
            }
            return Ok(());
        }
        if self.shown && now < self.next_refresh {
            return Ok(());
        }
        if !self.shown {
            self.meter.sample(now, totals); // start measuring from now
        }
        let rates = self.meter.sample(now, totals);
        let labels = Labels {
            stream: &self.stream,
            decoder: &self.decoder,
        };
        let lines = overlay_lines(
            &labels,
            rates,
            &stats,
            (self.rtt)(),
            self.frame_time.take_average(),
        );
        streamer.sink_mut().set_overlay(Some(lines))?;
        self.shown = true;
        self.next_refresh = now + OVERLAY_REFRESH;
        Ok(())
    }
}

/// Control-bar state handed from the window thread to the render loop.
/// `dirty` is set on every change so the loop applies it exactly once.
#[derive(Default)]
struct ToolbarSlot {
    view: Mutex<Option<ToolbarView>>,
    dirty: AtomicBool,
    /// Mirrors "the view is expanded" for the loop's cheap per-iteration
    /// check.
    expanded: AtomicBool,
}

impl ToolbarSlot {
    fn set(&self, view: Option<ToolbarView>) {
        let mut slot = self.view.lock().unwrap_or_else(|e| e.into_inner());
        self.expanded
            .store(view.as_ref().is_some_and(|v| v.expanded), Ordering::Relaxed);
        *slot = view;
        self.dirty.store(true, Ordering::Release);
    }

    /// The bar is open: the loop should wake often.
    fn active(&self) -> bool {
        self.expanded.load(Ordering::Relaxed)
    }

    /// The new view if it changed since the last call (outer `Some`).
    fn take_change(&self) -> Option<Option<ToolbarView>> {
        if !self.dirty.swap(false, Ordering::Acquire) {
            return None;
        }
        Some(self.view.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }
}

/// Width and height in one atomic word, so readers never see a torn pair.
fn pack(width: u32, height: u32) -> u64 {
    (u64::from(width) << 32) | u64::from(height)
}

fn unpack(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(expanded: bool) -> ToolbarView {
        ToolbarView {
            buttons: vec!["A".into()],
            tooltips: vec!["Tip".into()],
            expanded,
            hovered: None,
        }
    }

    #[test]
    fn toolbar_slot_reports_each_change_once() {
        let slot = ToolbarSlot::default();
        assert_eq!(slot.take_change(), None);
        assert!(!slot.active());
        slot.set(Some(view(true)));
        assert!(slot.active());
        assert_eq!(slot.take_change(), Some(Some(view(true))));
        assert_eq!(slot.take_change(), None);
        slot.set(Some(view(false)));
        assert!(!slot.active(), "a collapsed handle needs no fast wake-ups");
        slot.set(Some(view(true))); // coalesced: only the latest is seen
        assert_eq!(slot.take_change(), Some(Some(view(true))));
        slot.set(None);
        assert!(!slot.active());
        assert_eq!(slot.take_change(), Some(None));
    }

    #[test]
    fn size_packing_roundtrips() {
        for (w, h) in [(0, 0), (1920, 1080), (u32::MAX, 1), (7, u32::MAX)] {
            assert_eq!(unpack(pack(w, h)), (w, h));
        }
    }
}
