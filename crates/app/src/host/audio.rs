//! The sound of one session: a thread captures what this PC plays (WASAPI
//! loopback) and sends it to the viewer as audio datagrams, on the same
//! connection as the video, while both the host's "Condividi audio" switch
//! and the viewer's own Audio button want it (see `sender`).
//!
//! The thread polls every [`POLL`]: a chunk (10 ms) leaves at most that
//! long after it is complete. Audio datagrams never wait for the video
//! pipeline, and the video never waits for them: the encoder only sees
//! [`AudioLoad`], the bandwidth the sound takes, and keeps its own bitrate
//! below what remains.

mod sender;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use audio::{ComApartment, DefaultDevice, LoopbackCapture};
use net::VideoChannel;
use protocol::audio::AUDIO_BANDWIDTH_BPS;
use tokio::sync::watch;

use super::events::EventSink;
use crate::realtime::{MmcssRegistration, TimerResolution};
use sender::{AudioSender, SoundCapture, SoundDevices};

/// Time between two turns while capturing.
const POLL: Duration = Duration::from_millis(5);
/// Time between two looks at the switches while not capturing.
const IDLE_POLL: Duration = Duration::from_millis(20);

/// Whether the sound is being sent, for the video's bitrate: shared by the
/// audio thread (which sets it) and the streaming thread (which reads it).
#[derive(Debug, Clone, Default)]
pub(super) struct AudioLoad(Arc<AtomicBool>);

impl AudioLoad {
    fn set(&self, sending: bool) {
        self.0.store(sending, Ordering::Relaxed);
    }

    /// The bandwidth the sound takes now, in bit/s.
    pub(super) fn reserved_bps(&self) -> u32 {
        if self.0.load(Ordering::Relaxed) {
            AUDIO_BANDWIDTH_BPS
        } else {
            0
        }
    }
}

/// What the audio thread works with.
pub(super) struct AudioPlan {
    /// The connection the datagrams go to.
    pub(super) sink: VideoChannel,
    /// The host's "Condividi audio" switch, live.
    pub(super) shared: watch::Receiver<bool>,
    pub(super) load: AudioLoad,
    pub(super) events: EventSink,
}

/// Owns the audio thread of one session.
pub(super) struct SessionAudio {
    viewer_wants: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SessionAudio {
    /// Starts the thread; the viewer wants the sound until it says
    /// otherwise.
    pub(super) fn start(plan: AudioPlan) -> Result<Self> {
        let viewer_wants = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (viewer_wants, stop) = (viewer_wants.clone(), stop.clone());
            std::thread::Builder::new()
                .name("wshare-audio".into())
                .spawn(move || run(plan, &viewer_wants, &stop))
                .context("spawning the audio thread")?
        };
        Ok(Self {
            viewer_wants,
            stop,
            thread: Some(thread),
        })
    }

    /// The viewer's Audio button: on, or muted.
    pub(super) fn set_viewer_wants(&self, on: bool) {
        self.viewer_wants.store(on, Ordering::Relaxed);
    }

    /// Stops the thread and waits for it, without blocking the runtime.
    pub(super) async fn stop(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        let thread = self.thread.take().context("audio thread joined twice")?;
        tokio::task::spawn_blocking(move || thread.join())
            .await?
            .map_err(|_| anyhow!("the audio thread panicked"))
    }
}

impl Drop for SessionAudio {
    /// A session that ends early must not leave the thread capturing: it is
    /// told to stop and detached.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Body of the audio thread, until told to stop or the connection can no
/// longer send (the session then ends on its own).
fn run(plan: AudioPlan, viewer_wants: &AtomicBool, stop: &AtomicBool) {
    let AudioPlan {
        sink,
        shared,
        load,
        events,
    } = plan;
    let _com = ComApartment::enter();
    let _timer = TimerResolution::raise();
    let _mmcss = MmcssRegistration::register();
    let mut sender = AudioSender::new(Wasapi, sink, events);
    while !stop.load(Ordering::Relaxed) {
        let wanted = *shared.borrow() && viewer_wants.load(Ordering::Relaxed);
        let Ok(sending) = sender.step(wanted, Instant::now()) else {
            break;
        };
        load.set(sending);
        std::thread::sleep(if sending { POLL } else { IDLE_POLL });
    }
    load.set(false);
}

/// This PC's sound devices, through WASAPI.
struct Wasapi;

impl SoundDevices for Wasapi {
    type Capture = LoopbackCapture;

    fn open(&self) -> Result<LoopbackCapture> {
        LoopbackCapture::open(&DefaultDevice::new()?)
    }

    fn default_id(&self) -> Result<String> {
        DefaultDevice::new()?.id()
    }
}

impl SoundCapture for LoopbackCapture {
    fn read(&mut self, out: &mut Vec<i16>) -> Result<()> {
        LoopbackCapture::read(self, out)
    }

    fn device(&self) -> &str {
        LoopbackCapture::device(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sound_reserves_its_bandwidth_only_while_sent() {
        let load = AudioLoad::default();
        assert_eq!(load.reserved_bps(), 0);
        load.clone().set(true);
        assert_eq!(load.reserved_bps(), AUDIO_BANDWIDTH_BPS);
        load.set(false);
        assert_eq!(load.reserved_bps(), 0);
    }
}
