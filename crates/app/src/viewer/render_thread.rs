//! The thread that owns decoder and renderer for one viewing session.

use std::sync::Arc;
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
use render::Renderer;
use windows::Win32::Foundation::HWND;

use super::Outgoing;
use super::sink::ViewSink;

/// How often the loop wakes up without datagrams (keyframe retries, resize,
/// stop flag).
const TICK: Duration = Duration::from_millis(50);

/// Everything the render thread needs, moved into it at start.
pub struct RenderInput {
    /// The window handle as an integer: `HWND` itself is not `Send`.
    pub hwnd: isize,
    pub stream: StreamInfo,
    pub datagrams: Receiver<Bytes>,
    pub outgoing: Outgoing,
}

pub struct RenderThread {
    stop: Arc<AtomicBool>,
    size: Arc<AtomicU64>,
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
        let thread = {
            let (stop, size) = (stop.clone(), size.clone());
            std::thread::Builder::new()
                .name("wshare-render".into())
                .spawn(move || {
                    let result = render_loop(input, &stop, &size);
                    if !stop.load(Ordering::Relaxed) {
                        on_exit(&result);
                    }
                    result
                })
                .expect("spawning the render thread")
        };
        Self { stop, size, thread: Some(thread) }
    }

    pub fn resize(&self, width: u32, height: u32) {
        self.size.store(pack(width, height), Ordering::Relaxed);
    }

    /// Stops and joins the thread; must run before the window is destroyed.
    pub fn stop(&mut self) -> Option<Result<ViewerStats>> {
        self.stop.store(true, Ordering::Relaxed);
        let thread = self.thread.take()?;
        Some(thread.join().unwrap_or_else(|_| Err(anyhow!("render thread panicked"))))
    }
}

impl Drop for RenderThread {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn render_loop(input: RenderInput, stop: &AtomicBool, size: &AtomicU64) -> Result<ViewerStats> {
    let _mf = MediaFoundation::start()?;
    // Decoder and renderer must share one device so decoded textures can
    // be drawn directly.
    let gpu = Gpu::default_adapter()?;
    let codec = crate::wire::from_wire(input.stream.codec);
    let decoder = HwDecoder::new(&gpu, codec, input.stream.width, input.stream.height)?;
    let mut applied = unpack(size.load(Ordering::Relaxed));
    let renderer = Renderer::new(&gpu, HWND(input.hwnd as *mut _), applied.0, applied.1)?;
    println!("Decoding with {} on {}", decoder.name(), gpu.adapter_name);
    let mut streamer = ViewerStreamer::new(decoder, ViewSink::new(renderer));

    while !stop.load(Ordering::Relaxed) {
        let action = match input.datagrams.recv_timeout(TICK) {
            Ok(datagram) => streamer.on_datagram(&datagram, Instant::now())?,
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
    }
    Ok(streamer.stats().clone())
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

    #[test]
    fn size_packing_roundtrips() {
        for (w, h) in [(0, 0), (1920, 1080), (u32::MAX, 1), (7, u32::MAX)] {
            assert_eq!(unpack(pack(w, h)), (w, h));
        }
    }
}
