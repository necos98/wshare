//! Controller behaviour: turning unattended access on and off, and the
//! window leaving the sharing of this PC to the service.

use super::fakes::{Shared, TestController, calls, controller, controller_from};
use super::*;
use crate::gui::ports::UnattendedOutcome;

fn installed(installed: bool) -> (TestController, Shared) {
    let log = Shared::default();
    log.borrow_mut().unattended_installed = installed;
    (controller_from(&log), log)
}

#[test]
fn the_state_of_the_service_is_read_at_start() {
    let (c, _) = installed(true);
    assert!(c.state().unattended.shares_this_pc());
    let (c, _) = installed(false);
    assert_eq!(c.state().unattended.installed, Some(false));

    let log = Shared::default();
    log.borrow_mut().unattended_unreadable = true;
    let c = controller_from(&log);
    assert_eq!(c.state().unattended.installed, None);
    assert!(c.state().unattended.notice.is_some());
}

#[test]
fn while_the_service_shares_this_pc_the_window_does_not() {
    let (mut c, log) = installed(true);
    c.handle(Intent::SetSharing(true));
    let sharing = &c.state().sharing;
    assert!(!sharing.enabled);
    assert_eq!(sharing.error.as_deref(), Some(text::SHARING_BY_SERVICE));
    assert!(log.borrow().started.is_empty(), "the port is the service's");
}

#[test]
fn turning_it_on_stops_the_window_sharing_first_and_hands_over_the_password() {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    calls(&log);
    c.handle(Intent::EnableUnattended {
        password: "  segreto ".into(),
    });
    assert_eq!(
        calls(&log),
        [
            "stop_in_background",
            "unattended install 7420 Some(\"segreto\")"
        ]
    );
    assert!(c.state().unattended.busy());

    c.handle(Intent::EnableUnattended {
        password: String::new(),
    });
    assert!(calls(&log).is_empty(), "one change at a time");

    log.borrow_mut().unattended_installed = true;
    c.on_event(AppEvent::Unattended(UnattendedOutcome::Done));
    assert!(!c.state().unattended.busy());
    assert!(c.state().unattended.shares_this_pc());
    assert_eq!(
        c.state().unattended.notice.as_deref(),
        Some(text::UNATTENDED_ENABLED)
    );
}

#[test]
fn the_service_gets_the_host_settings_of_the_window() {
    let (mut c, log) = controller();
    let private = Settings {
        share_clipboard: false,
        share_audio: false,
        port: 9000,
        ..c.state().settings.clone()
    };
    c.handle(Intent::ChangeSettings(private.clone()));
    c.handle(Intent::EnableUnattended {
        password: String::new(),
    });
    assert_eq!(
        log.borrow().unattended_settings,
        Some(private.service_settings())
    );
    let sent = log.borrow().unattended_settings.clone().unwrap();
    assert!(!sent.share_clipboard, "the clipboard stays private");
}

#[test]
fn changing_a_setting_of_the_service_asks_for_an_update_until_it_is_done() {
    let (mut c, log) = installed(true);
    let current = |c: &TestController| c.state().settings.service_settings();
    assert!(!c.state().unattended.needs_update(&current(&c)));

    let private = Settings {
        share_clipboard: false,
        ..c.state().settings.clone()
    };
    c.handle(Intent::ChangeSettings(private));
    assert!(c.state().unattended.needs_update(&current(&c)));

    c.handle(Intent::EnableUnattended {
        password: String::new(),
    });
    c.on_event(AppEvent::Unattended(UnattendedOutcome::Done));
    assert!(log.borrow().unattended_installed);
    assert!(!c.state().unattended.needs_update(&current(&c)));
}

#[test]
fn an_empty_password_means_none() {
    let (mut c, log) = controller();
    c.handle(Intent::EnableUnattended {
        password: "   ".into(),
    });
    assert_eq!(calls(&log), ["unattended install 7420 None"]);
}

#[test]
fn turning_it_off_lets_the_window_share_again() {
    let (mut c, log) = installed(true);
    c.handle(Intent::DisableUnattended);
    assert_eq!(calls(&log), ["unattended uninstall"]);

    log.borrow_mut().unattended_installed = false;
    c.on_event(AppEvent::Unattended(UnattendedOutcome::Done));
    assert_eq!(
        c.state().unattended.notice.as_deref(),
        Some(text::UNATTENDED_DISABLED)
    );
    c.handle(Intent::SetSharing(true));
    assert!(c.state().sharing.enabled);
}

#[test]
fn a_cancelled_request_changes_nothing() {
    let (mut c, log) = controller();
    c.handle(Intent::EnableUnattended {
        password: String::new(),
    });
    calls(&log);
    c.on_event(AppEvent::Unattended(UnattendedOutcome::Cancelled));
    assert!(!c.state().unattended.shares_this_pc());
    assert_eq!(
        c.state().unattended.notice.as_deref(),
        Some(text::UNATTENDED_CANCELLED)
    );
}
