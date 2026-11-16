//! End-to-end checks of `HostHandle` against the real service. No viewer
//! connects, so nothing is ever injected into this PC.

use std::net::UdpSocket;
use std::time::Duration;

use super::events::recording::{next, recorder};
use super::{HostEvent, HostHandle};
use crate::access_mode::AccessMode;
use crate::options::{HostOptions, StreamOptions};

const WAIT: Duration = Duration::from_secs(10);

fn options(monitor: usize) -> HostOptions {
    HostOptions {
        stream: StreamOptions {
            monitor,
            ..StreamOptions::default()
        },
        port: 0, // ephemeral: never collides with a real host on 7420
        pin: Some("123456".into()),
        view_only: false,
        share_clipboard: true,
        access: AccessMode::PinOnly,
        receive_files: true,
        share_audio: true,
        downloads: None,
    }
}

#[test]
fn handle_can_move_to_a_ui_thread() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<HostHandle>();
}

#[test]
fn a_missing_monitor_fails_start_synchronously_without_events() {
    let (observer, events) = recorder();
    let error = HostHandle::start(options(99), observer)
        .err()
        .expect("monitor 99 should not exist");
    assert!(format!("{error:#}").contains("monitor 99"), "{error:#}");
    assert!(events.try_recv().is_err(), "setup errors are not events");
}

#[test]
#[ignore = "requires desktop (monitor 0, DXGI) and briefly announces on the LAN"]
fn start_rotate_pin_and_stop() {
    let (observer, events) = recorder();
    let handle = HostHandle::start(options(0), observer).expect("start");

    let HostEvent::Listening {
        port,
        pin,
        fingerprint,
        ..
    } = next(&events, WAIT)
    else {
        panic!("first event must be Listening");
    };
    assert_ne!(port, 0);
    assert_eq!(pin, "123456");
    assert!(!fingerprint.is_empty());

    // Possibly a Warning (LAN announcement) in between; skip it.
    handle.set_input_allowed(false);
    handle.disconnect_viewer(); // idle: no-op
    let new_pin = handle.new_pin();
    assert_ne!(new_pin, "");
    loop {
        match next(&events, WAIT) {
            HostEvent::Warning(_) => continue,
            HostEvent::PinChanged { pin } => {
                assert_eq!(pin, new_pin);
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    handle.stop();
    assert_eq!(next(&events, WAIT), HostEvent::Stopped { error: None });
    assert!(events.try_recv().is_err(), "nothing after Stopped");
    // The endpoint is gone with the service: its port is free again.
    UdpSocket::bind(("0.0.0.0", port)).expect("port released after stop");
}

#[test]
#[ignore = "requires desktop (monitor 0, DXGI) and briefly announces on the LAN"]
fn dropping_the_handle_stops_the_service() {
    let (observer, events) = recorder();
    let handle = HostHandle::start(options(0), observer).expect("start");
    assert!(matches!(next(&events, WAIT), HostEvent::Listening { .. }));
    drop(handle);
    loop {
        match next(&events, WAIT) {
            HostEvent::Warning(_) => continue,
            event => {
                assert_eq!(event, HostEvent::Stopped { error: None });
                break;
            }
        }
    }
}
