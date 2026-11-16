//! Controller behaviour around remembered PCs: hosts this PC connects to
//! without a PIN, and viewers this PC lets in without one.

use protocol::control::PROTOCOL_VERSION;

use super::fakes::{Shared, TestController, calls, controller, controller_knowing, host, short};
use super::*;
use crate::gui::model::tests::{found, listening};
use crate::gui::remembered::tests::known;
use crate::gui::settings::ViewerDisplay;
use crate::trust::KnownHost;
use crate::viewer::report::FailureCause;

const OFFICE_IP: &str = "192.168.1.3";

/// A controller listing `UFFICIO`, which its store says remembers this PC.
fn with_remembered_office() -> (TestController, Shared, KnownHost, String) {
    let office = known(0xab, "UFFICIO");
    let id = office.fingerprint.host_id();
    let (mut c, log) = controller_knowing(vec![office.clone()]);
    c.on_event(AppEvent::Discovery(found(
        &id,
        "UFFICIO",
        OFFICE_IP,
        PROTOCOL_VERSION,
    )));
    (c, log, office, id)
}

fn exited(cause: FailureCause) -> ViewerUpdate {
    ViewerUpdate::Exited {
        success: false,
        code: Some(cause.exit_code()),
        cause: Some(cause),
    }
}

fn last_launch(log: &Shared) -> LaunchRequest {
    log.borrow()
        .launched
        .last()
        .expect("a viewer was launched")
        .1
        .clone()
}

#[test]
fn known_hosts_are_loaded_at_start() {
    let (c, _log, _office, id) = with_remembered_office();
    assert!(c.state().host_row(&id).unwrap().remembered);
}

#[test]
fn a_remembered_host_connects_without_a_pin() {
    let (mut c, log, _office, id) = with_remembered_office();
    c.handle(Intent::ConnectHost {
        id: id.clone(),
        pin: String::new(),
    });
    assert_eq!(
        last_launch(&log),
        LaunchRequest {
            target: format!("{OFFICE_IP}:7420").parse().unwrap(),
            host_id: Some(id),
            pin: None,
            display: ViewerDisplay::default(),
        }
    );
    assert_eq!(c.state().connect_error, None);
}

#[test]
fn pin_required_shows_the_pin_field_and_the_pin_reconnects() {
    let (mut c, log, _office, id) = with_remembered_office();
    c.handle(Intent::ConnectHost {
        id: id.clone(),
        pin: String::new(),
    });
    c.on_event(AppEvent::Viewer {
        id: 1,
        update: exited(FailureCause::PinRequired),
    });
    assert!(!c.state().host_row(&id).unwrap().remembered);
    assert_eq!(c.state().selected_host.as_deref(), Some(id.as_str()));
    assert_eq!(c.state().connect_error.as_deref(), Some(text::PIN_AGAIN));

    c.handle(Intent::ConnectHost {
        id: id.clone(),
        pin: String::new(),
    });
    assert_eq!(
        log.borrow().launched.len(),
        1,
        "no second try without a PIN"
    );
    assert!(c.state().connect_error.as_deref().unwrap().contains("PIN"));

    c.handle(Intent::ConnectHost {
        id: id.clone(),
        pin: "123456".into(),
    });
    assert_eq!(last_launch(&log).pin.as_deref(), Some("123456"));
    c.on_event(AppEvent::Viewer {
        id: 2,
        update: ViewerUpdate::Connected,
    });
    assert!(
        c.state().host_row(&id).unwrap().remembered,
        "remembered again"
    );
}

#[test]
fn known_hosts_are_reloaded_when_a_viewer_connects() {
    let (mut c, log) = controller();
    let office = known(0xcd, "UFFICIO");
    let id = office.fingerprint.host_id();
    c.on_event(AppEvent::Discovery(found(
        &id,
        "UFFICIO",
        OFFICE_IP,
        PROTOCOL_VERSION,
    )));
    c.handle(Intent::ConnectHost {
        id: id.clone(),
        pin: "123456".into(),
    });
    assert!(!c.state().host_row(&id).unwrap().remembered);
    // The viewer process writes the store, then reports it connected.
    log.borrow_mut().known.push(office);
    c.on_event(AppEvent::Viewer {
        id: 1,
        update: ViewerUpdate::Connected,
    });
    assert!(c.state().host_row(&id).unwrap().remembered);
}

#[test]
fn forgetting_a_host_asks_for_the_pin_again() {
    let (mut c, log, office, id) = with_remembered_office();
    c.handle(Intent::ForgetHost { id: id.clone() });
    assert_eq!(
        calls(&log),
        [format!("forget_host {}", short(&office.fingerprint))]
    );
    assert!(!c.state().host_row(&id).unwrap().remembered);
    assert_eq!(c.state().connect_error, None);

    c.handle(Intent::ForgetHost { id });
    assert!(calls(&log).is_empty(), "nothing left to forget");
}

#[test]
fn a_failed_forget_is_shown() {
    let (mut c, log, _office, id) = with_remembered_office();
    log.borrow_mut().fail_forget = true;
    c.handle(Intent::ForgetHost { id: id.clone() });
    let error = c.state().connect_error.clone().unwrap();
    assert!(error.contains("access denied"), "{error}");
    assert!(c.state().host_row(&id).unwrap().remembered);
}

#[test]
fn an_address_connects_with_or_without_a_pin() {
    let (mut c, log) = controller();
    c.handle(Intent::ConnectAddress {
        address: "10.0.0.5".into(),
        pin: "  ".into(),
    });
    let request = last_launch(&log);
    assert_eq!(request.pin, None);
    assert_eq!(request.host_id, None);
    c.on_event(AppEvent::Viewer {
        id: 1,
        update: exited(FailureCause::PinRequired),
    });
    assert_eq!(
        c.state().viewers[0].status_line(),
        "10.0.0.5: Questo computer richiede il PIN."
    );
}

#[test]
fn removing_an_authorized_device_reaches_the_service() {
    let (mut c, log) = controller();
    let laptop = known(0x42, "PORTATILE").fingerprint;
    c.handle(Intent::RemoveDevice(laptop));
    assert!(calls(&log).is_empty(), "nothing to do while not sharing");

    c.handle(Intent::SetSharing(true));
    c.on_event(host(1, listening("111111", "ff")));
    calls(&log);
    c.handle(Intent::RemoveDevice(laptop));
    assert_eq!(calls(&log), ["forget_device 42"]);
}
