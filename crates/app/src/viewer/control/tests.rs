//! Reconnection against a scripted host on loopback, with real QUIC
//! connections: what the viewer retries, what ends it, and what it sends.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use net::{HostEndpoint, Identity};
use protocol::control::{ControlMessage, RejectReason, StreamInfo, VideoCodec};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use super::*;

const WAIT: Duration = Duration::from_secs(10);
const PIN: &str = "123456";

fn stream(codec: VideoCodec) -> StreamInfo {
    StreamInfo {
        codec,
        width: 1920,
        height: 1080,
        fps: 60,
        encoder: "test".into(),
    }
}

/// What the scripted host answers to one connection attempt.
#[derive(Clone)]
enum Answer {
    Welcome(VideoCodec),
    Reject(RejectReason),
}

/// A host answering each connection with the next scripted answer (the last
/// one repeats), and noting the PIN each `Hello` carried.
struct ScriptedHost {
    address: SocketAddr,
    identity: Identity,
    pins: Arc<Mutex<Vec<Option<String>>>>,
    serving: JoinHandle<()>,
    _endpoint: Arc<HostEndpoint>,
}

impl ScriptedHost {
    fn start(script: Vec<Answer>) -> Self {
        let identity = Identity::generate("host").unwrap();
        let endpoint =
            Arc::new(HostEndpoint::bind((Ipv4Addr::LOCALHOST, 0).into(), &identity).unwrap());
        let address = endpoint.local_addr().unwrap();
        let pins = Arc::new(Mutex::new(Vec::new()));
        let serving = {
            let (endpoint, pins) = (Arc::clone(&endpoint), Arc::clone(&pins));
            tokio::spawn(async move {
                let mut sessions = Vec::new();
                let mut turn = 0;
                while let Ok(session) = endpoint.accept().await {
                    let answer = script[turn.min(script.len() - 1)].clone();
                    turn += 1;
                    let (mut tx, mut rx, video) = session.split();
                    let Ok(Some(ControlMessage::Hello { pin, .. })) = rx.recv().await else {
                        continue;
                    };
                    pins.lock().unwrap().push(pin);
                    let reply = match answer {
                        Answer::Welcome(codec) => ControlMessage::Welcome {
                            host_name: "HOST".into(),
                            stream: stream(codec),
                            input_allowed: true,
                        },
                        Answer::Reject(reason) => ControlMessage::Rejected { reason },
                    };
                    tx.send(&reply).await.unwrap();
                    // Keep the connection up: dropping it would close it.
                    sessions.push((tx, rx, video));
                }
            })
        };
        Self {
            address,
            identity,
            pins,
            serving,
            _endpoint: endpoint,
        }
    }

    fn attempts(&self) -> usize {
        self.pins.lock().unwrap().len()
    }

    fn plan(&self, pin: Option<&str>) -> Reconnection {
        Reconnection {
            addr: self.address,
            host: self.identity.fingerprint(),
            stream: stream(VideoCodec::H264),
            identity: None,
            pin: pin.map(String::from),
        }
    }
}

impl Drop for ScriptedHost {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

fn lost() -> ViewerFailure {
    ViewerFailure::ConnectionLost("idle".into())
}

async fn reestablish_in_time(
    plan: &Reconnection,
    outgoing: &mut tokio::sync::mpsc::UnboundedReceiver<ControlMessage>,
) -> Result<Connected, Stop> {
    tokio::time::timeout(WAIT, reestablish(lost(), outgoing, plan))
        .await
        .expect("reestablish did not finish")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_that_is_back_is_connected_again_at_once() {
    let host = ScriptedHost::start(vec![Answer::Welcome(VideoCodec::H264)]);
    let (_keep, mut outgoing) = unbounded_channel();
    let started = Instant::now();
    let connected = reestablish_in_time(&host.plan(None), &mut outgoing)
        .await
        .unwrap();
    assert_eq!(connected.fingerprint, host.identity.fingerprint());
    assert_eq!(host.attempts(), 1);
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "first try at once"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_host_still_holding_the_old_session_is_tried_again() {
    let host = ScriptedHost::start(vec![
        Answer::Reject(RejectReason::Busy),
        Answer::Welcome(VideoCodec::H264),
    ]);
    let (_keep, mut outgoing) = unbounded_channel();
    let started = Instant::now();
    reestablish_in_time(&host.plan(None), &mut outgoing)
        .await
        .unwrap();
    assert_eq!(host.attempts(), 2);
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "the second attempt waits for the first backoff step"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pin_the_host_asks_for_ends_the_reconnection_without_more_attempts() {
    for reason in [RejectReason::PinRequired, RejectReason::WrongPin] {
        let host = ScriptedHost::start(vec![Answer::Reject(reason.clone())]);
        let (_keep, mut outgoing) = unbounded_channel();
        let stop = reestablish_in_time(&host.plan(None), &mut outgoing)
            .await
            .err()
            .expect("must not connect");
        assert_eq!(stop, Stop::Failed(ViewerFailure::Rejected(reason)));
        assert_eq!(host.attempts(), 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pin_from_the_command_line_is_sent_again_but_nothing_else_is() {
    let host = ScriptedHost::start(vec![Answer::Welcome(VideoCodec::H264)]);
    let (_keep, mut outgoing) = unbounded_channel();
    reestablish_in_time(&host.plan(Some(PIN)), &mut outgoing)
        .await
        .unwrap();
    assert_eq!(*host.pins.lock().unwrap(), [Some(PIN.to_string())]);

    let host = ScriptedHost::start(vec![Answer::Welcome(VideoCodec::H264)]);
    reestablish_in_time(&host.plan(None), &mut outgoing)
        .await
        .unwrap();
    assert_eq!(*host.pins.lock().unwrap(), [None]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_user_can_disconnect_while_reconnecting_and_stale_input_is_dropped() {
    // Nobody listens: the attempts fail or hang, the user gives up.
    let host = ScriptedHost::start(vec![Answer::Welcome(VideoCodec::H264)]);
    let plan = Reconnection {
        addr: (Ipv4Addr::LOCALHOST, 9).into(),
        ..host.plan(None)
    };
    let (sender, mut outgoing) = unbounded_channel();
    let stale = [
        ControlMessage::RequestKeyframe,
        ControlMessage::Clipboard("stale".into()),
    ];
    for message in stale {
        sender.send(message).unwrap();
    }
    cancel_later(&sender);
    let stop = reestablish_in_time(&plan, &mut outgoing)
        .await
        .err()
        .expect("must not connect");
    assert_eq!(stop, Stop::Cancelled);
    assert_eq!(host.attempts(), 0);
}

fn cancel_later(sender: &UnboundedSender<ControlMessage>) {
    let sender = sender.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        sender.send(ControlMessage::Bye).unwrap();
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_window_cancels_too() {
    let host = ScriptedHost::start(vec![Answer::Welcome(VideoCodec::H264)]);
    let plan = Reconnection {
        addr: (Ipv4Addr::LOCALHOST, 9).into(),
        ..host.plan(None)
    };
    let (sender, mut outgoing) = unbounded_channel::<ControlMessage>();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(sender);
    });
    let stop = reestablish_in_time(&plan, &mut outgoing)
        .await
        .err()
        .expect("must not connect");
    assert_eq!(stop, Stop::Cancelled);
}

#[tokio::test]
async fn until_cancelled_drops_everything_but_the_goodbye() {
    let (sender, mut outgoing) = unbounded_channel();
    sender.send(ControlMessage::RequestKeyframe).unwrap();
    sender
        .send(ControlMessage::Clipboard("old".into()))
        .unwrap();
    sender.send(ControlMessage::Bye).unwrap();
    sender.send(ControlMessage::RequestKeyframe).unwrap();
    until_cancelled(&mut outgoing).await;
    assert!(
        outgoing.try_recv().is_ok(),
        "only what came before the goodbye was dropped"
    );
}

#[test]
fn giving_up_ends_as_a_lost_connection_whatever_the_last_error_was() {
    let reported = |last| match gave_up(&last) {
        ViewerFailure::ConnectionLost(text) => text,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        reported(ViewerFailure::Unreachable("refused".into())),
        "no connection again within 30 s (refused)"
    );
    assert_eq!(reported(lost()), "no connection again within 30 s (idle)");
}

#[test]
fn an_error_that_is_not_a_known_failure_counts_as_a_lost_connection() {
    let known: anyhow::Error = ViewerFailure::HostLeft.into();
    assert_eq!(failure_of(known), ViewerFailure::HostLeft);
    assert_eq!(
        failure_of(anyhow::anyhow!("socket")),
        ViewerFailure::ConnectionLost("socket".into())
    );
}

#[test]
fn the_window_hears_of_monitors_and_stream_changes() {
    let monitors = vec![protocol::session::MonitorEntry::new("D1", 1920, 1080, true)];
    let Some(UserEvent::Monitors {
        monitors: told,
        current,
    }) = window_event(ControlMessage::MonitorList {
        monitors: monitors.clone(),
        current: 0,
    })
    else {
        panic!("monitors not passed on")
    };
    assert_eq!((told, current), (monitors, 0));
    let changed = stream(VideoCodec::Hevc);
    assert!(matches!(
        window_event(ControlMessage::StreamChanged(changed.clone())),
        Some(UserEvent::StreamChanged(stream)) if stream == changed
    ));
    assert!(matches!(
        window_event(ControlMessage::CursorVisible(false)),
        Some(UserEvent::CursorVisible(false))
    ));
    assert!(window_event(ControlMessage::RequestKeyframe).is_none());
}

#[test]
fn only_a_quality_other_than_automatic_is_asked_for_again() {
    assert_eq!(quality_request(Quality::Automatic), None);
    assert_eq!(
        quality_request(Quality::Low),
        Some(ControlMessage::SetQuality(Quality::Low))
    );
}
