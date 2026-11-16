//! Controller behaviour: checking for a newer wshare and installing it.

use super::fakes::{Shared, TestController, controller_from};
use super::*;
use crate::gui::ports::UpdateEvent;
use crate::gui::settings::Settings;
use crate::gui::update_state::UpdatePhase;
use crate::update::{Available, UpdateFailure, test_available, test_manifest};

fn release(version: &str) -> Available {
    test_available(test_manifest(version, &[("wshare.exe", b"exe")]))
}

fn with(settings: Settings) -> (TestController, Shared) {
    let log = Shared::default();
    log.borrow_mut().settings = settings;
    (controller_from(&log), log)
}

fn update_calls(log: &Shared) -> Vec<String> {
    log.borrow().update_calls.clone()
}

/// A controller that found `version`.
fn found(version: &str) -> (TestController, Shared) {
    let (mut c, log) = with(Settings::default());
    c.handle(Intent::CheckUpdates);
    c.on_event(AppEvent::Update(UpdateEvent::Checked(Ok(Some(release(
        version,
    ))))));
    (c, log)
}

#[test]
fn start_up_checks_only_if_the_settings_allow_it() {
    let (mut c, log) = with(Settings::default());
    c.start_up();
    assert_eq!(update_calls(&log), ["check"]);
    assert_eq!(c.state().update.phase, UpdatePhase::Checking);

    let (mut c, log) = with(Settings {
        check_updates: false,
        ..Settings::default()
    });
    c.start_up();
    assert!(update_calls(&log).is_empty());
}

#[test]
fn a_check_shows_the_newer_version_or_that_this_is_the_latest() {
    let (c, _) = found("0.2.0");
    assert_eq!(
        c.state().update.available().map(Available::version),
        Some("0.2.0")
    );

    let (mut c, _) = with(Settings::default());
    c.handle(Intent::CheckUpdates);
    c.on_event(AppEvent::Update(UpdateEvent::Checked(Ok(None))));
    assert_eq!(c.state().update.phase, UpdatePhase::UpToDate);
}

#[test]
fn a_failed_check_says_why_and_can_be_repeated() {
    let (mut c, log) = with(Settings::default());
    c.handle(Intent::CheckUpdates);
    c.handle(Intent::CheckUpdates); // still checking: not twice
    c.on_event(AppEvent::Update(UpdateEvent::Checked(Err(
        UpdateFailure::BadSignature,
    ))));
    assert!(c.state().update.notice.is_some());
    c.handle(Intent::CheckUpdates);
    assert_eq!(update_calls(&log), ["check", "check"]);
}

#[test]
fn installing_restarts_on_the_new_program_and_quits() {
    let (mut c, log) = found("0.2.0");
    c.handle(Intent::SetSharing(true));
    c.handle(Intent::InstallUpdate);
    assert!(c.state().update.busy());
    let command = c.on_event(AppEvent::Update(UpdateEvent::Installed(Ok(()))));
    assert_eq!(command, Some(WindowCommand::Quit));
    assert_eq!(update_calls(&log), ["check", "install 0.2.0", "restart"]);
    assert!(
        !c.state().sharing.enabled,
        "the sharing stopped before the new wshare starts"
    );
}

#[test]
fn a_failed_installation_keeps_the_release_to_try_again() {
    let (mut c, _) = found("0.2.0");
    c.handle(Intent::InstallUpdate);
    let failure = UpdateFailure::FolderNotWritable(r"C:\Program Files\wshare".into());
    let command = c.on_event(AppEvent::Update(UpdateEvent::Installed(Err(failure))));
    assert_eq!(command, None);
    assert!(c.state().update.available().is_some());
    assert!(
        c.state()
            .update
            .notice
            .as_deref()
            .unwrap()
            .contains("Program Files")
    );
}

#[test]
fn if_the_new_program_cannot_start_the_window_stays_and_says_so() {
    let (mut c, log) = found("0.2.0");
    log.borrow_mut().fail_restart = true;
    c.handle(Intent::InstallUpdate);
    let command = c.on_event(AppEvent::Update(UpdateEvent::Installed(Ok(()))));
    assert_eq!(command, None);
    assert_eq!(
        c.state().update.phase,
        UpdatePhase::RestartByHand("0.2.0".into())
    );
    assert!(c.state().update.notice.is_some());
}

#[test]
fn without_a_release_found_install_does_nothing() {
    let (mut c, log) = with(Settings::default());
    c.handle(Intent::InstallUpdate);
    assert!(update_calls(&log).is_empty());
}

#[test]
fn an_outdated_service_is_reported() {
    let log = Shared::default();
    log.borrow_mut().service_outdated = true;
    let c = controller_from(&log);
    assert!(c.state().update.service_outdated);
}
