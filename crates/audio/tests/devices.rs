//! The real sound card, when the machine has one. Without a playback
//! device opening fails with an error, never a panic: that is the case the
//! sessions rely on to go on without sound.

use std::time::{Duration, Instant};

use audio::{ComApartment, DefaultDevice, LoopbackCapture, Playback};
use protocol::audio::{CHANNELS, CHUNK_FRAMES};

const RUN: Duration = Duration::from_millis(200);
const POLL: Duration = Duration::from_millis(5);

#[test]
fn capture_and_playback_run_or_fail_cleanly() {
    let _com = ComApartment::enter();
    let devices = match DefaultDevice::new() {
        Ok(devices) => devices,
        Err(e) => return eprintln!("no device enumerator: {e:#}"),
    };
    let default = devices.id();
    match LoopbackCapture::open(&devices) {
        Ok(mut capture) => {
            assert_eq!(Some(capture.device()), default.as_deref().ok());
            let mut samples = Vec::new();
            let end = Instant::now() + RUN;
            while Instant::now() < end {
                capture.read(&mut samples).expect("reading the capture");
                std::thread::sleep(POLL);
            }
            // Nothing playing may deliver nothing; whatever came is frames.
            assert_eq!(samples.len() % CHANNELS, 0);
        }
        Err(e) => eprintln!("no loopback capture: {e:#}"),
    }
    match Playback::open(&devices) {
        Ok(mut playback) => {
            let free = playback.free_frames().unwrap();
            let frames = free.min(CHUNK_FRAMES as u32);
            playback
                .write(frames, |out| {
                    assert_eq!(out.len(), frames as usize * CHANNELS);
                    out.fill(0);
                })
                .unwrap();
            assert!(playback.queued_frames().unwrap() <= free);
        }
        Err(e) => eprintln!("no playback: {e:#}"),
    }
}
