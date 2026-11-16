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
