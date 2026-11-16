//! Controller behaviour around the request of a viewer to be accepted, the
//! access mode and a viewer waiting for the other computer.

use protocol::control::PROTOCOL_VERSION;

use super::fakes::{Shared, TestController, calls, controller, controller_from, host};
use super::*;
use crate::access_mode::AccessMode;
use crate::gui::model::tests::{found, listening};
use crate::gui::remembered::tests::known;
use crate::host::ApprovalRequest;
use crate::viewer::report::FailureCause;

const GENERATION: u64 = 1;

fn request(id: u64) -> HostEvent {
    HostEvent::ApprovalRequested(ApprovalRequest {
        id,
        viewer_name: "PORTATILE".into(),
        address: "192.168.1.7:5000".parse().unwrap(),
    })
}

/// A sharing PC that has just been asked to accept request 3.
fn asked() -> (TestController, Shared) {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    c.on_event(host(GENERATION, listening("123456", "abcd")));
    c.on_event(host(GENERATION, request(3)));
    calls(&log);
    (c, log)
}

#[test]
fn a_request_brings_the_window_up_and_shows_who_asks() {
    let (mut c, _log) = controller();
    c.handle(Intent::SetSharing(true));
    let command = c.on_event(host(GENERATION, request(3)));
    assert_eq!(command, Some(WindowCommand::BringToFront));
    let prompt = c.state().sharing.approval.as_ref().unwrap();
    assert_eq!(
        prompt.message(),
        "PORTATILE (192.168.1.7) vuole connettersi"
    );
    assert!(prompt.remember, "remembering is on by default");
}

#[test]
fn a_request_is_also_a_notification_when_notifications_are_on() {
    let (mut c, _log) = controller();
    c.handle(Intent::SetSharing(true));
    c.on_event(host(GENERATION, request(3)));
    let notifications = c.take_notifications();
    assert_eq!(notifications.len(), 1);
    assert_eq!(
        notifications[0].body,
        "PORTATILE (192.168.1.7) vuole connettersi"
    );

    let (mut quiet, _log) = controller_with(Settings {
        notifications: false,
        ..Settings::default()
    });
    quiet.handle(Intent::SetSharing(true));
    let command = quiet.on_event(host(GENERATION, request(3)));
    assert!(quiet.take_notifications().is_empty());
    assert_eq!(
        command,
        Some(WindowCommand::BringToFront),
        "the window asks anyway: the request needs an answer"
    );
}

fn controller_with(settings: Settings) -> (TestController, Shared) {
    let log = Shared::default();
    log.borrow_mut().settings = settings;
    (controller_from(&log), log)
}

#[test]
fn accepting_tells_the_service_and_takes_the_request_down() {
    let (mut c, log) = asked();
    c.handle(Intent::AnswerApproval { accept: true });
    assert_eq!(calls(&log), ["approval 3 Accept { remember: true }"]);
    assert_eq!(c.state().sharing.approval, None);
}

#[test]
fn the_remember_box_decides_whether_the_device_is_remembered() {
    let (mut c, log) = asked();
    c.handle(Intent::SetApprovalRemember(false));
    assert!(!c.state().sharing.approval.as_ref().unwrap().remember);
    c.handle(Intent::AnswerApproval { accept: true });
    assert_eq!(calls(&log), ["approval 3 Accept { remember: false }"]);
}

#[test]
fn refusing_never_accepts_whatever_the_remember_box_says() {
    let (mut c, log) = asked();
    c.handle(Intent::AnswerApproval { accept: false });
    assert_eq!(calls(&log), ["approval 3 Refuse"]);
    assert_eq!(c.state().sharing.approval, None);
}

#[test]
fn a_click_with_no_request_open_does_nothing() {
    let (mut c, log) = asked();
    c.handle(Intent::AnswerApproval { accept: false });
    calls(&log);
    c.handle(Intent::AnswerApproval { accept: true });
    c.handle(Intent::SetApprovalRemember(false));
    assert!(calls(&log).is_empty());
}

#[test]
fn a_request_that_ends_takes_the_prompt_down() {
    let (mut c, log) = asked();
    c.on_event(host(GENERATION, HostEvent::ApprovalClosed { id: 2 }));
    assert!(
        c.state().sharing.approval.is_some(),
        "another request ended, not this one"
    );
    c.on_event(host(GENERATION, HostEvent::ApprovalClosed { id: 3 }));
    assert_eq!(c.state().sharing.approval, None);
    c.handle(Intent::AnswerApproval { accept: true });
    assert!(calls(&log).is_empty(), "nothing is left to answer");
}

#[test]
fn stopping_the_sharing_takes_the_prompt_down() {
    let (mut c, _log) = asked();
    c.handle(Intent::SetSharing(false));
    assert_eq!(c.state().sharing.approval, None);
}

#[test]
fn the_access_mode_reaches_the_running_service_only_when_it_changes() {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    calls(&log);

    let mut settings = c.state().settings.clone();
    settings.access = AccessMode::PinOrConfirmation;
    c.handle(Intent::ChangeSettings(settings));
    assert_eq!(calls(&log), ["access PinOrConfirmation"]);
    assert!(!c.state().restart_needed(), "it applies at once");
    assert_eq!(
        log.borrow().saved.last().unwrap().access,
        AccessMode::PinOrConfirmation
    );

    let mut settings = c.state().settings.clone();
    settings.fps = 30;
    c.handle(Intent::ChangeSettings(settings));
    assert!(calls(&log).is_empty(), "other settings do not touch it");
}

#[test]
fn the_sharing_starts_with_the_saved_access_mode_and_pin_only_by_default() {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    assert_eq!(log.borrow().started[0].0.access, AccessMode::PinOnly);

    let (mut c, log) = controller_with(Settings {
        access: AccessMode::Confirmation,
        ..Settings::default()
    });
    c.handle(Intent::SetSharing(true));
    assert_eq!(log.borrow().started[0].0.access, AccessMode::Confirmation);
}

/// A controller listing `UFFICIO`, which does not know this PC yet.
fn with_listed_office() -> (TestController, Shared, String) {
    let (mut c, log) = controller();
    let id = known(0xab, "UFFICIO").fingerprint.host_id();
    c.on_event(AppEvent::Discovery(found(
        &id,
        "UFFICIO",
        "192.168.1.3",
        PROTOCOL_VERSION,
    )));
    (c, log, id)
}

#[test]
fn asking_for_access_connects_without_a_pin() {
    let (mut c, log, id) = with_listed_office();
    c.handle(Intent::RequestAccess { id: id.clone() });
    let launched = log.borrow().launched.clone();
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].1.pin, None);
    assert_eq!(c.state().connect_error, None);

    c.handle(Intent::RequestAccess {
        id: "no-such-host".into(),
    });
    assert_eq!(c.state().connect_error.as_deref(), Some(text::HOST_GONE));
}

#[test]
fn connecting_with_an_empty_pin_still_asks_for_the_pin() {
    let (mut c, log, id) = with_listed_office();
    c.handle(Intent::ConnectHost {
        id,
        pin: "  ".into(),
    });
    assert_eq!(c.state().connect_error.as_deref(), Some(text::PIN_MISSING));
    assert!(log.borrow().launched.is_empty());
}

#[test]
fn a_viewer_waiting_for_the_other_computer_says_so_and_can_be_cancelled() {
    let (mut c, log, id) = with_listed_office();
    c.handle(Intent::RequestAccess { id });
    c.on_event(AppEvent::Viewer {
        id: 1,
        update: ViewerUpdate::AwaitingApproval,
    });
    let viewer = &c.state().viewers[0];
    assert_eq!(viewer.status, ViewerStatus::AwaitingApproval);
    assert_eq!(
        viewer.status_line(),
        "UFFICIO: In attesa di conferma sull'altro computer…"
    );
    assert!(viewer.is_active());

    c.handle(Intent::CancelViewer(1));
    assert!(c.state().viewers.is_empty());
    assert_eq!(calls(&log).last().map(String::as_str), Some("cancel 1"));
}

#[test]
fn a_viewer_that_was_accepted_connects_after_waiting() {
    let (mut c, _log, id) = with_listed_office();
    c.handle(Intent::RequestAccess { id });
    c.on_event(AppEvent::Viewer {
        id: 1,
        update: ViewerUpdate::AwaitingApproval,
    });
    c.on_event(AppEvent::Viewer {
        id: 1,
        update: ViewerUpdate::Connected,
    });
    assert_eq!(c.state().viewers[0].status, ViewerStatus::Connected);
    // A late report of the wait does not take the connection back.
    c.on_event(AppEvent::Viewer {
        id: 1,
        update: ViewerUpdate::AwaitingApproval,
    });
    assert_eq!(c.state().viewers[0].status, ViewerStatus::Connected);
}

#[test]
fn a_refused_or_expired_request_says_so_on_the_viewer_line() {
    for (cause, message) in [
        (
            FailureCause::Denied,
            "UFFICIO: L'altro computer ha rifiutato la connessione.",
        ),
        (
            FailureCause::ApprovalExpired,
            "UFFICIO: Nessuno ha risposto sull'altro computer: la richiesta è scaduta.",
        ),
        (
            FailureCause::TooManyRequests,
            "UFFICIO: Troppe richieste di accesso: attendi un minuto e riprova.",
        ),
    ] {
        let (mut c, _log, id) = with_listed_office();
        c.handle(Intent::RequestAccess { id });
        c.on_event(AppEvent::Viewer {
            id: 1,
            update: ViewerUpdate::AwaitingApproval,
        });
        c.on_event(AppEvent::Viewer {
            id: 1,
            update: ViewerUpdate::Exited {
                success: false,
                code: Some(cause.exit_code()),
                cause: Some(cause),
            },
        });
        let viewer = &c.state().viewers[0];
        assert_eq!(viewer.status_line(), message);
        assert!(!viewer.is_active());
    }
}
