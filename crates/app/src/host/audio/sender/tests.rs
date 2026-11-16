//! Tests of [`super::AudioSender`] with fake devices and a fake connection.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc::Receiver;

use anyhow::{anyhow, bail};
use bytes::Bytes;
use protocol::audio::{AudioPacket, CHUNK_FRAMES, CHUNK_SAMPLES};

use super::*;
use crate::host::events::recording::recorder;

/// What a fake capture delivers, step by step: samples, or a failure.
type Feed = Rc<RefCell<VecDeque<Result<Vec<i16>, String>>>>;

struct FakeCapture {
    device: String,
    feed: Feed,
}

impl SoundCapture for FakeCapture {
    fn read(&mut self, out: &mut Vec<i16>) -> Result<()> {
        match self.feed.borrow_mut().pop_front() {
            Some(Ok(samples)) => {
                out.extend(samples);
                Ok(())
            }
            Some(Err(error)) => Err(anyhow!(error)),
            None => Ok(()),
        }
    }

    fn device(&self) -> &str {
        &self.device
    }
}

#[derive(Default)]
struct Devices {
    /// The default device; `None`: there is none.
    default: RefCell<Option<String>>,
    opened: RefCell<Vec<String>>,
    feed: Feed,
}

impl SoundDevices for Rc<Devices> {
    type Capture = FakeCapture;

    fn open(&self) -> Result<FakeCapture> {
        let Some(device) = self.default.borrow().clone() else {
            bail!("no audio playback device");
        };
        self.opened.borrow_mut().push(device.clone());
        Ok(FakeCapture {
            device,
            feed: self.feed.clone(),
        })
    }

    fn default_id(&self) -> Result<String> {
        self.default
            .borrow()
            .clone()
            .ok_or_else(|| anyhow!("no audio playback device"))
    }
}

/// What the fake connection was given.
#[derive(Default)]
struct Sent {
    datagrams: RefCell<Vec<Bytes>>,
    broken: RefCell<bool>,
}

#[derive(Clone, Default)]
struct Sink(Rc<Sent>);

impl DatagramSink for Sink {
    fn max_datagram_size(&self) -> usize {
        1200
    }

    fn send(&self, datagram: Bytes) -> Result<()> {
        if *self.0.broken.borrow() {
            bail!("connection lost");
        }
        self.0.datagrams.borrow_mut().push(datagram);
        Ok(())
    }

    fn backlog_bytes(&self) -> usize {
        0
    }
}

struct Fixture {
    sender: AudioSender<Rc<Devices>, Sink>,
    devices: Rc<Devices>,
    sink: Sink,
    events: Receiver<HostEvent>,
    start: Instant,
}

impl Fixture {
    fn new(device: Option<&str>) -> Self {
        let devices = Rc::new(Devices::default());
        *devices.default.borrow_mut() = device.map(str::to_string);
        let sink = Sink::default();
        let (observer, events) = recorder();
        let sender = AudioSender::new(devices.clone(), sink.clone(), EventSink::new(observer));
        Self {
            sender,
            devices,
            sink,
            events,
            start: Instant::now(),
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.start + Duration::from_millis(ms)
    }

    fn deliver(&self, samples: Vec<i16>) {
        self.devices.feed.borrow_mut().push_back(Ok(samples));
    }

    /// Stream and position of every datagram sent so far, which it takes.
    fn sent(&self) -> Vec<(u16, u64)> {
        self.sink
            .0
            .datagrams
            .borrow_mut()
            .drain(..)
            .map(|datagram| {
                let packet = AudioPacket::parse(&datagram).unwrap();
                (packet.stream, packet.position)
            })
            .collect()
    }

    fn warnings(&self) -> Vec<HostWarning> {
        self.events
            .try_iter()
            .filter_map(|event| match event {
                HostEvent::Warning(warning) => Some(warning),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn whole_chunks_are_sent_as_audio_datagrams_and_a_partial_one_waits() {
    let mut f = Fixture::new(Some("speakers"));
    f.deliver(vec![1; CHUNK_SAMPLES + 10]);
    assert!(f.sender.step(true, f.at(0)).unwrap());
    // 10 ms do not fit in one datagram: two, each with its position.
    let half = CHUNK_FRAMES as u64 / 2;
    assert_eq!(f.sent(), [(0, 0), (0, half)]);
    f.deliver(vec![2; CHUNK_SAMPLES - 20]);
    assert!(f.sender.step(true, f.at(5)).unwrap());
    assert!(f.sent().is_empty(), "10 samples short of a chunk");
    f.deliver(vec![3; 10]);
    f.sender.step(true, f.at(10)).unwrap();
    let chunk = CHUNK_FRAMES as u64;
    assert_eq!(f.sent(), [(0, chunk), (0, chunk + half)]);
    assert!(f.warnings().is_empty());
}

#[test]
fn nothing_is_captured_while_the_sound_is_not_wanted() {
    let mut f = Fixture::new(Some("speakers"));
    f.deliver(vec![1; CHUNK_SAMPLES]);
    assert!(!f.sender.step(false, f.at(0)).unwrap());
    assert!(f.devices.opened.borrow().is_empty());
    assert!(f.sent().is_empty());

    assert!(f.sender.step(true, f.at(10)).unwrap());
    assert_eq!(f.sent()[0].0, 0);
    // Muted, then wanted again: a new capture, a new stream.
    assert!(!f.sender.step(false, f.at(20)).unwrap());
    f.deliver(vec![1; CHUNK_SAMPLES]);
    assert!(f.sender.step(true, f.at(30)).unwrap());
    assert_eq!(f.sent(), [(1, 0), (1, CHUNK_FRAMES as u64 / 2)]);
    assert_eq!(f.devices.opened.borrow().len(), 2);
}

#[test]
fn without_a_device_the_session_goes_on_silent_with_one_warning() {
    let mut f = Fixture::new(None);
    assert!(!f.sender.step(true, f.at(0)).unwrap());
    assert!(matches!(
        f.warnings().as_slice(),
        [HostWarning::AudioUnavailable(detail)] if detail.contains("no audio playback device")
    ));
    // Not tried again before the pause, and not reported again after it.
    assert!(!f.sender.step(true, f.at(500)).unwrap());
    let retry = RETRY.as_millis() as u64;
    assert!(!f.sender.step(true, f.at(retry)).unwrap());
    assert!(f.warnings().is_empty());
    // Headphones plugged in: the sound comes, with the next attempt.
    *f.devices.default.borrow_mut() = Some("headphones".into());
    assert!(!f.sender.step(true, f.at(retry + 100)).unwrap());
    assert!(f.sender.step(true, f.at(2 * retry)).unwrap());
    assert_eq!(*f.devices.opened.borrow(), ["headphones"]);
    // Unplugged again: reported again.
    *f.devices.default.borrow_mut() = None;
    f.devices
        .feed
        .borrow_mut()
        .push_back(Err("device invalidated".into()));
    assert!(!f.sender.step(true, f.at(2 * retry + 10)).unwrap());
    assert_eq!(f.warnings().len(), 1);
}

#[test]
fn a_new_default_device_is_followed() {
    let mut f = Fixture::new(Some("speakers"));
    f.sender.step(true, f.at(0)).unwrap();
    *f.devices.default.borrow_mut() = Some("headphones".into());
    let check = DEVICE_CHECK.as_millis() as u64;
    f.sender.step(true, f.at(check - 1)).unwrap();
    assert_eq!(
        f.devices.opened.borrow().len(),
        1,
        "looked at once a second"
    );
    f.deliver(vec![1; CHUNK_SAMPLES]);
    f.sender.step(true, f.at(check)).unwrap();
    assert_eq!(*f.devices.opened.borrow(), ["speakers", "headphones"]);
    assert_eq!(f.sent()[0], (1, 0), "a new stream from the new device");
}

#[test]
fn a_failing_capture_is_opened_again_at_once() {
    let mut f = Fixture::new(Some("speakers"));
    f.sender.step(true, f.at(0)).unwrap();
    f.devices
        .feed
        .borrow_mut()
        .push_back(Err("device invalidated".into()));
    assert!(f.sender.step(true, f.at(5)).unwrap());
    assert_eq!(f.devices.opened.borrow().len(), 2);
    assert!(f.warnings().is_empty());
}

#[test]
fn a_connection_that_cannot_send_ends_the_sound() {
    let mut f = Fixture::new(Some("speakers"));
    *f.sink.0.broken.borrow_mut() = true;
    f.deliver(vec![1; CHUNK_SAMPLES]);
    assert!(f.sender.step(true, f.at(0)).is_err());
}
