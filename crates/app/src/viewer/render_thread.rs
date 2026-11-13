//! The thread that owns decoder and renderer for one viewing session.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use bytes::Bytes;
use codec::{HwDecoder, MediaFoundation};
use gpu::Gpu;
use pipeline::{ViewerAction, ViewerStats, ViewerStreamer};
use protocol::control::{ControlMessage, StreamInfo};
use render::{Renderer, ToolbarView};
use windows::Win32::Foundation::HWND;

use super::Outgoing;
use super::sink::ViewSink;
use super::stats::{FrameTimer, Labels, RateMeter, Totals, overlay_lines};

/// How often the loop wakes up without datagrams (keyframe retries, resize,
/// stop flag, overlay refresh).
const TICK: Duration = Duration::from_millis(50);
/// Wake-up period while the control bar exists (full screen), so hover and
/// collapse changes show within a frame or two. Idle windows keep [`TICK`].
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
    thread: Option<JoinHandle<Result<ViewerStats>>>,
}

impl RenderThread {
    /// `on_exit` runs on the render thread when it ends by itself (error).
    pub fn start(
        input: RenderInput,
        width: u32,
        height: u32,
        on_exit: impl FnOnce(&Result<ViewerStats>) + Send + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let size = Arc::new(AtomicU64::new(pack(width, height)));
        let stats_visible = Arc::new(AtomicBool::new(false));
        let toolbar = Arc::new(ToolbarSlot::default());
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
                    };
                    let result = render_loop(input, shared);
                    if !stop.load(Ordering::Relaxed) {
                        on_exit(&result);
                    }
                    result
                })
                .expect("spawning the render thread")
        };
        Self {
            stop,
            size,
            stats_visible,
            toolbar,
            thread: Some(thread),
        }
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
}

fn render_loop(input: RenderInput, shared: Shared<'_>) -> Result<ViewerStats> {
    let Shared {
        stop,
        size,
        stats_visible,
        toolbar,
    } = shared;
    let _mf = MediaFoundation::start()?;
    // Decoder and renderer must share one device so decoded textures can
    // be drawn directly.
    let gpu = Gpu::default_adapter()?;
    let codec = crate::wire::from_wire(input.stream.codec);
    let decoder = HwDecoder::new(&gpu, codec, input.stream.width, input.stream.height)?;
    let mut applied = unpack(size.load(Ordering::Relaxed));
    let renderer = Renderer::new(&gpu, HWND(input.hwnd as *mut _), applied.0, applied.1)?;
    println!("Decoding with {} on {}", decoder.name(), gpu.adapter_name);
    let mut overlay =
        StatsOverlay::new(input.stream.clone(), decoder.name().to_string(), input.rtt);
    let mut streamer = ViewerStreamer::new(decoder, ViewSink::new(renderer));

    while !stop.load(Ordering::Relaxed) {
        let action =
            match input
                .datagrams
                .recv_timeout(if toolbar.active() { TOOLBAR_TICK } else { TICK })
            {
                Ok(datagram) => {
                    let (started, shown) = (Instant::now(), streamer.stats().frames_presented);
                    let action = streamer.on_datagram(&datagram, started)?;
                    overlay.on_datagram(
                        datagram.len(),
                        streamer.stats().frames_presented > shown,
                        started,
                    );
                    action
                }
                Err(RecvTimeoutError::Timeout) => streamer.on_tick(Instant::now()),
                Err(RecvTimeoutError::Disconnected) => break,
            };
        if action == ViewerAction::RequestKeyframe {
            let _ = input.outgoing.send(ControlMessage::RequestKeyframe);
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

    /// presented: the datagram completed a frame that is now on screen;
    /// the time since started is then its decode + present time.
    fn on_datagram(&mut self, len: usize, presented: bool, started: Instant) {
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
    /// Mirrors `view.is_some()` for the loop's cheap per-iteration check.
    present: AtomicBool,
}

impl ToolbarSlot {
    fn set(&self, view: Option<ToolbarView>) {
        let mut slot = self.view.lock().unwrap_or_else(|e| e.into_inner());
        self.present.store(view.is_some(), Ordering::Relaxed);
        *slot = view;
        self.dirty.store(true, Ordering::Release);
    }

    /// A bar exists (full screen): the loop should wake often.
    fn active(&self) -> bool {
        self.present.load(Ordering::Relaxed)
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
