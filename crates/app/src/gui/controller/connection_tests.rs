//! Controller behaviour around the life of a connection: a wrong PIN, giving
//! up while connecting, and how a session ends.

use protocol::control::PROTOCOL_VERSION;

use super::fakes::{Shared, TestController, calls, controller};
use super::*;
use crate::gui::model::tests::found;
use crate::gui::remembered::tests::known;
use crate::viewer::report::FailureCause;

const OFFICE_IP: &str = "192.168.1.3";

/// A controller listing `UFFICIO`, which does not know this PC yet.
fn with_listed_office() -> (TestController, Shared, String) {
    let (mut c, log) = controller();
    let id = known(0xab, "UFFICIO").fingerprint.host_id();
    c.on_event(AppEvent::Discovery(found(
        &id,
        "UFFICIO",
        OFFICE_IP,
        PROTOCOL_VERSION,
    )));
    (c, log, id)
}

fn exited(id: ViewerId, cause: FailureCause) -> AppEvent {
    AppEvent::Viewer {
        id,
        update: ViewerUpdate::Exited {
            success: false,
            code: Some(cause.exit_code()),
            cause: Some(cause),
        },
    }
}

fn connect(c: &mut TestController, id: &str, pin: &str) {
    c.handle(Intent::ConnectHost {
        id: id.into(),
        pin: pin.into(),
    });
}

#[test]
fn a_wrong_pin_keeps_the_host_selected_and_the_next_try_needs_no_reselecting() {
    let (mut c, log, id) = with_listed_office();
    c.handle(Intent::SelectHost(Some(id.clone())));
    connect(&mut c, &id, "111111");
    assert_eq!(
        c.state().selected_host,
        None,
        "the form closes while connecting"
    );

    let prompts = c.state().pin_prompts;
    c.on_event(exited(1, FailureCause::WrongPin));
    let state = c.state();
    assert_eq!(state.selected_host.as_deref(), Some(id.as_str()));
    assert_eq!(state.connect_error.as_deref(), Some(text::PIN_WRONG));
    assert!(state.viewers.is_empty(), "the error is under the host row");
    assert_eq!(state.pin_prompts, prompts + 1);

    connect(&mut c, &id, "222222");
    let launched = log.borrow().launched.clone();
    assert_eq!(launched.len(), 2);
    assert_eq!(launched[1].1.pin.as_deref(), Some("222222"));
    assert_eq!(c.state().connect_error, None, "a new try clears the error");
}

#[test]
fn cancelling_a_connection_ends_the_viewer_and_removes_the_entry() {
    let (mut c, log, id) = with_listed_office();
    connect(&mut c, &id, "111111");
    calls(&log);

    c.handle(Intent::CancelViewer(1));
    assert_eq!(calls(&log), ["cancel 1"]);
    assert!(c.state().viewers.is_empty());

    // The process ends a moment later: nothing comes back from it.
    c.on_event(exited(1, FailureCause::Other("killed".into())));
    assert!(c.state().viewers.is_empty());
}

#[test]
fn cancelling_leaves_other_viewers_and_connected_ones_alone() {
    let (mut c, log, id) = with_listed_office();
    connect(&mut c, &id, "111111");
    c.handle(Intent::ConnectAddress {
        address: "10.0.0.5".into(),
        pin: "1".into(),
    });
    c.on_event(AppEvent::Viewer {
        id: 1,
        update: ViewerUpdate::Connected,
    });
    calls(&log);

    c.handle(Intent::CancelViewer(1));
    c.handle(Intent::CancelViewer(99));
    assert!(
        calls(&log).is_empty(),
        "connected or unknown: nothing to end"
    );
    assert_eq!(c.state().viewers.len(), 2);

    c.handle(Intent::CancelViewer(2));
    assert_eq!(calls(&log), ["cancel 2"]);
    assert_eq!(c.state().viewers.len(), 1);
}

#[test]
fn a_cancelled_connection_can_be_started_again() {
    let (mut c, log, id) = with_listed_office();
    connect(&mut c, &id, "111111");
    c.handle(Intent::CancelViewer(1));
    c.handle(Intent::SelectHost(Some(id.clone())));
    connect(&mut c, &id, "222222");
    assert_eq!(log.borrow().launched.len(), 2);
    assert_eq!(c.state().viewers.len(), 1);
}

#[test]
fn a_session_that_ends_says_why_in_the_sessions_section() {
    let cases = [
        (
            FailureCause::HostLeft,
            "UFFICIO: Il computer ha interrotto la condivisione.",
        ),
        (
            FailureCause::HostDisconnected,
            "UFFICIO: Disconnesso dall'altro computer.",
        ),
        (FailureCause::ConnectionLost, "UFFICIO: Connessione persa."),
    ];
    for (cause, line) in cases {
        let (mut c, _log, id) = with_listed_office();
        connect(&mut c, &id, "111111");
        c.on_event(AppEvent::Viewer {
            id: 1,
            update: ViewerUpdate::Connected,
        });
        c.on_event(exited(1, cause));
        assert_eq!(c.state().viewers.len(), 1, "not removed in silence");
        assert_eq!(c.state().viewers[0].status_line(), line);
    }
}

#[test]
fn closing_the_window_yourself_leaves_no_entry() {
    let (mut c, _log, id) = with_listed_office();
    connect(&mut c, &id, "111111");
    c.on_event(AppEvent::Viewer {
        id: 1,
        update: ViewerUpdate::Connected,
    });
    c.on_event(AppEvent::Viewer {
        id: 1,
        update: ViewerUpdate::Exited {
            success: true,
            code: Some(0),
            cause: None,
        },
    });
    assert!(c.state().viewers.is_empty());
}
