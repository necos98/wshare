//! Controller behaviour around the settings: loading, saving, applying,
//! the Windows start-up entry, restarting the sharing, balloons.

use super::fakes::{Shared, TestController, calls, controller, controller_from, host};
use super::*;
use crate::gui::model::tests::listening;
use crate::gui::notifications::Notification;
use crate::gui::ports::MonitorChoice;
use crate::gui::settings::{CodecChoice, ViewerDisplay};
use crate::host::HostEvent;

/// A controller whose settings file holds `settings`.
fn controller_with_settings(settings: Settings) -> (TestController, Shared) {
    let log = Shared::default();
    log.borrow_mut().settings = settings;
    (controller_from(&log), log)
}

fn changed(c: &mut TestController, edit: impl FnOnce(&mut Settings)) {
    let mut settings = c.state().settings.clone();
    edit(&mut settings);
    c.handle(Intent::ChangeSettings(settings));
}

fn viewer_connected(name: &str) -> HostEvent {
    HostEvent::ViewerConnected {
        viewer_name: name.into(),
        address: "192.168.1.7:5000".parse().unwrap(),
    }
}

fn viewer_left() -> HostEvent {
    HostEvent::ViewerDisconnected {
        address: "192.168.1.7:5000".parse().unwrap(),
        stats: Default::default(),
        error: None,
    }
}

#[test]
fn saved_settings_are_loaded_at_start() {
    let saved = Settings {
        fps: 30,
        allow_input: false,
        ..Settings::default()
    };
    let (c, log) = controller_with_settings(saved.clone());
    assert_eq!(c.state().settings, saved);
    assert!(!c.state().sharing.input_allowed);
    assert_eq!(c.state().settings_notice, None);
    assert!(log.borrow().saved.is_empty(), "loading does not write");
}

#[test]
fn an_unreadable_settings_file_is_warned_about_and_does_not_stop_the_app() {
    let log = Shared::default();
    log.borrow_mut().settings_warning = Some("the file is damaged".into());
    let c = controller_from(&log);
    assert_eq!(c.state().settings, Settings::default());
    assert_eq!(
        c.state().settings_notice.as_deref(),
        Some("Impostazioni non leggibili, uso quelle predefinite: the file is damaged")
    );
}

#[test]
fn a_change_is_saved_and_an_identical_one_is_not() {
    let (mut c, log) = controller();
    changed(&mut c, |s| s.fps = 30);
    assert_eq!(c.state().settings.fps, 30);
    assert_eq!(log.borrow().saved.len(), 1);
    assert_eq!(log.borrow().saved[0].fps, 30);

    changed(&mut c, |s| s.fps = 30);
    assert_eq!(log.borrow().saved.len(), 1, "nothing changed");
}

#[test]
fn out_of_range_values_are_clamped_before_saving() {
    let (mut c, log) = controller();
    changed(&mut c, |s| {
        s.fps = 0;
        s.max_mbps = 1_000_000;
    });
    assert_eq!(c.state().settings.fps, 1);
    assert_eq!(c.state().settings.max_mbps, 500);
    assert_eq!(log.borrow().saved[0], c.state().settings);
}

#[test]
fn a_save_failure_is_shown_and_the_change_still_applies() {
    let (mut c, log) = controller();
    log.borrow_mut().fail_save = true;
    changed(&mut c, |s| s.notifications = false);
    assert!(!c.state().settings.notifications);
    assert_eq!(
        c.state().settings_notice.as_deref(),
        Some("Impostazioni non salvate: disk full")
    );
    log.borrow_mut().fail_save = false;
    changed(&mut c, |s| s.notifications = true);
    assert_eq!(c.state().settings_notice, None, "the next save clears it");
}

#[test]
fn sharing_starts_with_the_saved_streaming_settings() {
    let (mut c, log) = controller_with_settings(Settings {
        monitor: 1,
        fps: 30,
        max_mbps: 8,
        codec: CodecChoice::Hevc,
        port: 9000,
        ..Settings::default()
    });
    c.handle(Intent::SetSharing(true));
    let (options, _) = log.borrow().started[0].clone();
    assert_eq!(options, c.state().settings.host_options());
    assert_eq!(options.stream.monitor, 1);
    assert_eq!(options.port, 9000);
}

#[test]
fn sharing_on_start_is_honoured_by_the_start_up() {
    let (mut c, log) = controller_with_settings(Settings {
        share_on_start: true,
        ..Settings::default()
    });
    assert!(!c.state().sharing.enabled, "not before the window exists");
    c.start_up();
    assert!(c.state().sharing.enabled);
    assert_eq!(log.borrow().started.len(), 1);

    let (mut c, log) = controller();
    c.start_up();
    assert!(!c.state().sharing.enabled);
    assert!(log.borrow().started.is_empty());
    assert!(calls(&log).is_empty(), "the registry is left alone");
}

#[test]
fn the_registry_is_touched_only_when_the_option_is_on() {
    let (mut c, log) = controller();
    changed(&mut c, |s| s.fps = 30);
    changed(&mut c, |s| s.start_minimized = true);
    assert!(calls(&log).is_empty());

    changed(&mut c, |s| s.start_with_windows = true);
    assert_eq!(calls(&log), ["autostart true true"]);

    changed(&mut c, |s| s.start_minimized = false);
    assert_eq!(calls(&log), ["autostart true false"], "the flag follows");

    changed(&mut c, |s| s.start_with_windows = false);
    assert_eq!(
        calls(&log),
        ["autostart false false"],
        "the entry is removed"
    );
}

#[test]
fn start_up_keeps_an_enabled_entry_pointing_at_this_exe() {
    let (mut c, log) = controller_with_settings(Settings {
        start_with_windows: true,
        start_minimized: true,
        ..Settings::default()
    });
    c.start_up();
    assert_eq!(calls(&log), ["autostart true true"]);
}

#[test]
fn a_failed_start_with_windows_change_goes_back_and_says_why() {
    let (mut c, log) = controller();
    log.borrow_mut().fail_autostart = true;
    changed(&mut c, |s| {
        s.start_with_windows = true;
        s.fps = 30;
    });
    let state = c.state();
    assert!(
        !state.settings.start_with_windows,
        "the entry was not written"
    );
    assert_eq!(state.settings.fps, 30, "the rest still applies");
    assert_eq!(
        state.settings_notice.as_deref(),
        Some("Impossibile cambiare l'avvio con Windows: registry is read-only")
    );
    assert!(!log.borrow().saved[0].start_with_windows, "and is saved so");
}

#[test]
fn a_failed_start_up_registration_is_reported_not_fatal() {
    let (mut c, log) = controller_with_settings(Settings {
        start_with_windows: true,
        ..Settings::default()
    });
    log.borrow_mut().fail_autostart = true;
    c.start_up();
    assert!(c.state().settings_notice.is_some());
}

#[test]
fn opening_the_settings_lists_the_monitors() {
    let (mut c, log) = controller();
    log.borrow_mut().monitors = vec![MonitorChoice {
        index: 0,
        label: "1 - DISPLAY1 (1920×1080)".into(),
    }];
    c.handle(Intent::OpenSettings);
    assert_eq!(c.state().page, Page::Settings);
    assert_eq!(c.state().monitors, log.borrow().monitors);
    c.handle(Intent::CloseSettings);
    assert_eq!(c.state().page, Page::Main);
}

#[test]
fn a_monitor_list_failure_still_opens_the_settings() {
    let (mut c, log) = controller();
    log.borrow_mut().fail_monitors = true;
    c.handle(Intent::OpenSettings);
    assert_eq!(c.state().page, Page::Settings);
    assert!(c.state().monitors.is_empty());
    assert_eq!(
        c.state().settings_notice.as_deref(),
        Some("Elenco dei monitor non disponibile: no display")
    );
}

#[test]
fn streaming_changes_wait_for_a_restart_and_remote_control_does_not() {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    calls(&log);

    changed(&mut c, |s| s.allow_input = false);
    assert_eq!(calls(&log), ["input false"], "live");
    assert!(!c.state().restart_needed());

    changed(&mut c, |s| s.fps = 30);
    assert!(c.state().restart_needed());
    assert_eq!(log.borrow().started.len(), 1, "nothing restarts by itself");
}

#[test]
fn restarting_stops_then_starts_with_the_new_settings() {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    changed(&mut c, |s| s.fps = 30);
    calls(&log);

    c.handle(Intent::RestartSharing);
    assert_eq!(calls(&log), ["stop_in_background"]);
    assert!(c.state().restarting);
    assert_eq!(log.borrow().started.len(), 1, "the port is still busy");

    c.on_event(AppEvent::SharingStopped { generation: 1 });
    assert_eq!(log.borrow().started.len(), 2);
    assert_eq!(log.borrow().started[1].0.stream.fps, 30);
    assert!(c.state().sharing.enabled);
    assert!(!c.state().restarting);
    assert!(!c.state().restart_needed());
}

#[test]
fn restarting_does_nothing_while_off() {
    let (mut c, log) = controller();
    c.handle(Intent::RestartSharing);
    assert!(log.borrow().started.is_empty());
    assert!(!c.state().restarting);
}

#[test]
fn quitting_cancels_a_restart() {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    c.handle(Intent::RestartSharing);
    c.handle(Intent::Quit);
    c.on_event(AppEvent::SharingStopped { generation: 1 });
    assert_eq!(log.borrow().started.len(), 1);
    assert!(!c.state().restarting);
}

#[test]
fn remote_control_survives_a_restart_of_the_app() {
    let (mut c, log) = controller();
    c.handle(Intent::SetInputAllowed(false));
    assert!(!log.borrow().saved.last().unwrap().allow_input);

    let (restarted, _log) = controller_with_settings(log.borrow().settings.clone());
    assert!(!restarted.state().sharing.input_allowed);
}

#[test]
fn viewers_open_the_way_the_settings_say() {
    let (mut c, log) = controller();
    changed(&mut c, |s| {
        s.viewer_fullscreen = true;
        s.viewer_view_only = true;
    });
    c.handle(Intent::ConnectAddress {
        address: "192.168.1.9".into(),
        pin: "123456".into(),
    });
    assert_eq!(
        log.borrow().launched[0].1.display,
        ViewerDisplay {
            view_only: true,
            fullscreen: true,
            stats: false
        }
    );
}

#[test]
fn tray_actions_reach_the_service() {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    c.on_event(host(1, listening("111111", "ff")));
    calls(&log);

    c.on_event(AppEvent::Tray(TrayAction::NewPin));
    assert_eq!(c.state().sharing.pin.as_deref(), Some("654321"));
    c.on_event(AppEvent::Tray(TrayAction::DisconnectViewer));
    c.on_event(AppEvent::Tray(TrayAction::ToggleInputAllowed));
    assert!(!c.state().settings.allow_input);
    c.on_event(AppEvent::Tray(TrayAction::ToggleInputAllowed));
    assert!(c.state().settings.allow_input);
    assert_eq!(
        calls(&log),
        ["new_pin", "disconnect", "input false", "input true"]
    );
    assert_eq!(log.borrow().saved.len(), 2, "the choice is kept");
}

#[test]
fn balloons_announce_who_comes_and_goes() {
    let (mut c, _log) = controller();
    c.handle(Intent::SetSharing(true));
    c.on_event(host(1, viewer_connected("PORTATILE")));
    c.on_event(host(1, viewer_left()));
    let bodies: Vec<_> = c
        .take_notifications()
        .into_iter()
        .map(|Notification { body, .. }| body)
        .collect();
    assert_eq!(
        bodies,
        ["PORTATILE si è connesso", "PORTATILE si è disconnesso"]
    );
    assert!(c.take_notifications().is_empty(), "each is shown once");
}

#[test]
fn balloons_are_silent_when_switched_off_or_for_a_previous_run() {
    let (mut c, _log) = controller();
    changed(&mut c, |s| s.notifications = false);
    c.handle(Intent::SetSharing(true));
    c.on_event(host(1, viewer_connected("PORTATILE")));
    assert!(c.take_notifications().is_empty());

    changed(&mut c, |s| s.notifications = true);
    c.on_event(host(7, viewer_left()));
    assert!(
        c.take_notifications().is_empty(),
        "late event of another run"
    );
}

#[test]
fn the_connection_indicator_follows_the_viewer_and_the_setting() {
    let (mut c, _log) = controller();
    c.handle(Intent::SetSharing(true));
    assert_eq!(c.state().indicator_text(), None);
    c.on_event(host(1, viewer_connected("PORTATILE")));
    assert_eq!(
        c.state().indicator_text().as_deref(),
        Some("PORTATILE è connesso")
    );
    changed(&mut c, |s| s.connection_indicator = false);
    assert_eq!(c.state().indicator_text(), None);
    changed(&mut c, |s| s.connection_indicator = true);
    c.on_event(host(1, viewer_left()));
    assert_eq!(c.state().indicator_text(), None);
}
