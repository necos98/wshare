//! Controller behaviour: sharing, the host list and viewer windows.

use std::net::SocketAddr;

use protocol::control::PROTOCOL_VERSION;

use super::fakes::{calls, controller, host};
use super::*;
use crate::gui::model::tests::{found, listening};
use crate::gui::settings::ViewerDisplay;
use crate::host::{HostEvent, HostFailure, HostWarning};
use crate::viewer::report::FailureCause;

#[test]
fn toggle_on_and_off() {
    let (mut c, log) = controller();
    assert_eq!(c.handle(Intent::SetSharing(true)), None);
    assert!(c.state().sharing.enabled);
    let (options, generation) = log.borrow().started[0].clone();
    assert_eq!(options.port, DEFAULT_PORT);
    assert!(!options.view_only);
    assert_eq!(calls(&log), ["input true"]);

    c.on_event(host(generation, listening("123456", "abcd")));
    assert_eq!(c.state().sharing.pin.as_deref(), Some("123456"));

    c.handle(Intent::SetSharing(false));
    assert!(!c.state().sharing.enabled);
    assert!(c.state().sharing.stopping);
    assert_eq!(c.state().sharing.pin, None);
    assert_eq!(
        calls(&log),
        ["stop_in_background"],
        "the UI thread does not wait"
    );
    c.handle(Intent::SetSharing(false));
    assert!(calls(&log).is_empty(), "already off");

    c.on_event(AppEvent::SharingStopped { generation });
    assert!(!c.state().sharing.stopping);
    assert_eq!(calls(&log), ["wait"], "the finished stop thread is joined");
    c.on_event(AppEvent::SharingStopped { generation });
    assert!(calls(&log).is_empty(), "a repeated report is ignored");
}

#[test]
fn setup_error_resets_toggle() {
    let (mut c, log) = controller();
    log.borrow_mut().fail_start = Some(HostFailure::PortInUse { port: 7420 });
    c.handle(Intent::SetSharing(true));
    let sharing = &c.state().sharing;
    assert!(!sharing.enabled);
    assert!(
        sharing
            .error
            .as_deref()
            .unwrap()
            .contains("porta 7420 è già in uso")
    );

    log.borrow_mut().fail_start = None;
    c.handle(Intent::ToggleSharing);
    assert!(c.state().sharing.enabled);
    assert_eq!(c.state().sharing.error, None);
}

#[test]
fn setup_errors_are_worded_by_their_cause() {
    let cases = [
        (
            HostFailure::MonitorNotFound { index: 1 },
            "Impossibile condividere: monitor non trovato.",
        ),
        (
            HostFailure::Crashed("boom".into()),
            "Il servizio di condivisione si è bloccato: boom",
        ),
    ];
    for (failure, message) in cases {
        let (mut c, log) = controller();
        log.borrow_mut().fail_start = Some(failure);
        c.handle(Intent::SetSharing(true));
        assert_eq!(c.state().sharing.error.as_deref(), Some(message));
    }
}

#[test]
fn service_stopping_by_itself_turns_toggle_off() {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    calls(&log);
    c.on_event(host(
        1,
        HostEvent::Stopped {
            error: Some(HostFailure::Failed("capture failed".into())),
        },
    ));
    assert!(!c.state().sharing.enabled);
    assert_eq!(
        c.state().sharing.error.as_deref(),
        Some("Impossibile condividere: capture failed")
    );
    assert_eq!(calls(&log), ["stop"], "the finished service is joined");
}

#[test]
fn events_of_a_previous_run_are_ignored() {
    let (mut c, _log) = controller();
    c.handle(Intent::SetSharing(true));
    c.handle(Intent::SetSharing(false));
    c.on_event(AppEvent::SharingStopped { generation: 1 });
    c.handle(Intent::SetSharing(true));
    c.on_event(host(1, HostEvent::Stopped { error: None }));
    c.on_event(AppEvent::SharingStopped { generation: 1 });
    assert!(c.state().sharing.enabled, "stale Stopped of run 1");
    c.on_event(host(2, listening("222222", "ff")));
    assert_eq!(c.state().sharing.pin.as_deref(), Some("222222"));
}

#[test]
fn new_pin_and_disconnect_reach_the_service() {
    let (mut c, log) = controller();
    c.handle(Intent::NewPin);
    c.handle(Intent::DisconnectViewer);
    assert!(calls(&log).is_empty(), "nothing to do while off");

    c.handle(Intent::SetSharing(true));
    c.on_event(host(1, listening("111111", "ff")));
    calls(&log);
    c.handle(Intent::NewPin);
    assert_eq!(c.state().sharing.pin.as_deref(), Some("654321"));
    c.handle(Intent::DisconnectViewer);
    assert_eq!(calls(&log), ["new_pin", "disconnect"]);
}

#[test]
fn a_pin_the_service_rotated_by_itself_is_shown_with_why() {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    c.on_event(host(1, listening("111111", "ff")));
    calls(&log);
    c.on_event(host(
        1,
        HostEvent::PinChanged {
            pin: "222222".into(),
        },
    ));
    c.on_event(host(
        1,
        HostEvent::Warning(HostWarning::PinRotatedAfterWrongAttempts),
    ));
    let sharing = &c.state().sharing;
    assert_eq!(sharing.pin.as_deref(), Some("222222"));
    assert_eq!(
        sharing.notice.as_deref(),
        Some("PIN cambiato dopo troppi tentativi errati.")
    );
    assert!(calls(&log).is_empty(), "nothing to ask the service");
}

#[test]
fn viewer_connect_and_disconnect_events() {
    let (mut c, _log) = controller();
    c.handle(Intent::SetSharing(true));
    let address: SocketAddr = "192.168.1.9:6000".parse().unwrap();
    c.on_event(host(1, listening("111111", "ff")));
    c.on_event(host(
        1,
        HostEvent::ViewerConnected {
            viewer_name: "PC2".into(),
            address,
        },
    ));
    assert_eq!(
        c.state().sharing.status_line(),
        "Connesso: PC2 (192.168.1.9)"
    );
    c.on_event(host(
        1,
        HostEvent::ViewerDisconnected {
            address,
            stats: Default::default(),
            error: None,
        },
    ));
    assert_eq!(c.state().sharing.status_line(), "In attesa di connessioni…");
}

#[test]
fn input_allowed_flag() {
    let (mut c, log) = controller();
    c.handle(Intent::SetInputAllowed(false));
    assert!(!c.state().sharing.input_allowed);
    assert!(calls(&log).is_empty());

    c.handle(Intent::SetSharing(true));
    assert!(
        log.borrow().started[0].0.view_only,
        "preference applies at start"
    );
    assert_eq!(calls(&log), ["input false"]);
    c.handle(Intent::SetInputAllowed(true));
    assert_eq!(calls(&log), ["input true"]);
    assert!(c.state().sharing.input_allowed);
}

#[test]
fn host_list_updates_and_discovery_errors() {
    let (mut c, _log) = controller();
    c.on_event(AppEvent::DiscoveryFailed("socket".into()));
    assert!(c.state().discovery_error.is_some());
    c.on_event(AppEvent::Discovery(found(
        "bbbb",
        "Ufficio",
        "192.168.1.3",
        PROTOCOL_VERSION,
    )));
    assert_eq!(c.state().discovery_error, None);
    assert_eq!(c.state().host_rows().len(), 1);
}

#[test]
fn connect_to_a_listed_host() {
    let (mut c, log) = controller();
    c.on_event(AppEvent::Discovery(found(
        "bbbb",
        "Ufficio",
        "192.168.1.3",
        PROTOCOL_VERSION,
    )));
    c.handle(Intent::SelectHost(Some("bbbb".into())));
    c.handle(Intent::ConnectHost {
        id: "bbbb".into(),
        pin: " 123456 ".into(),
    });
    let request = LaunchRequest {
        target: "192.168.1.3:7420".parse().unwrap(),
        host_id: None, // "bbbb" is not a well-formed host id
        pin: Some("123456".into()),
        display: ViewerDisplay::default(),
        downloads: None,
    };
    assert_eq!(log.borrow().launched, [(1, request)]);
    let viewer = &c.state().viewers[0];
    assert_eq!(viewer.status_line(), "Connessione a Ufficio in corso…");
    assert_eq!(c.state().selected_host, None);

    c.on_event(AppEvent::Viewer {
        id: 1,
        update: ViewerUpdate::Connected,
    });
    assert_eq!(c.state().viewers[0].status, ViewerStatus::Connected);

    c.handle(Intent::ConnectHost {
        id: "bbbb".into(),
        pin: "1".into(),
    });
    assert!(
        c.state()
            .connect_error
            .as_deref()
            .unwrap()
            .contains("già aperta")
    );
    assert_eq!(log.borrow().launched.len(), 1);
}

#[test]
fn viewer_failure_message_and_dismiss() {
    let (mut c, _log) = controller();
    c.handle(Intent::ConnectAddress {
        address: "10.0.0.5".into(),
        pin: "1".into(),
    });
    c.on_event(AppEvent::Viewer {
        id: 1,
        update: ViewerUpdate::Exited {
            success: false,
            code: Some(FailureCause::WrongPin.exit_code()),
            cause: Some(FailureCause::WrongPin),
        },
    });
    assert_eq!(c.state().viewers[0].status_line(), "10.0.0.5: PIN errato.");
    c.handle(Intent::DismissViewer(1));
    assert!(c.state().viewers.is_empty());
}

#[test]
fn a_busy_host_and_a_pause_are_told_apart_from_a_firewall() {
    let cases = [
        (
            FailureCause::HostBusy,
            "10.0.0.5: Computer già in uso da un altro visualizzatore: riprova quando si libera.",
        ),
        (
            FailureCause::TooManyAttempts,
            "10.0.0.5: Troppi PIN errati: attendi un minuto e riprova.",
        ),
    ];
    for (cause, line) in cases {
        let (mut c, _log) = controller();
        c.handle(Intent::ConnectAddress {
            address: "10.0.0.5".into(),
            pin: "1".into(),
        });
        c.on_event(AppEvent::Viewer {
            id: 1,
            update: ViewerUpdate::Exited {
                success: false,
                code: Some(cause.exit_code()),
                cause: Some(cause),
            },
        });
        assert_eq!(c.state().viewers[0].status_line(), line);
        assert!(!line.contains("firewall"));
    }
}

#[test]
fn retry_replaces_the_failed_entry() {
    let (mut c, log) = controller();
    c.handle(Intent::ConnectAddress {
        address: "10.0.0.5:9000".into(),
        pin: "1".into(),
    });
    c.on_event(AppEvent::Viewer {
        id: 1,
        update: ViewerUpdate::Exited {
            success: false,
            code: Some(1),
            cause: None,
        },
    });
    c.handle(Intent::ConnectAddress {
        address: "10.0.0.5:9000".into(),
        pin: "2".into(),
    });
    assert_eq!(c.state().viewers.len(), 1);
    assert_eq!(c.state().viewers[0].id, 2);
    let target: SocketAddr = "10.0.0.5:9000".parse().unwrap();
    assert_eq!(log.borrow().launched[1].1.target, target);
}

#[test]
fn launch_failure_is_shown() {
    let (mut c, log) = controller();
    log.borrow_mut().fail_launch = true;
    c.handle(Intent::ConnectAddress {
        address: "10.0.0.5".into(),
        pin: "1".into(),
    });
    let ViewerStatus::Failed(message) = &c.state().viewers[0].status else {
        panic!()
    };
    assert!(message.contains("file not found"), "{message}");
}

#[test]
fn invalid_connection_requests() {
    let (mut c, log) = controller();
    c.handle(Intent::ConnectAddress {
        address: "non-un-ip".into(),
        pin: "1".into(),
    });
    assert!(
        c.state()
            .connect_error
            .as_deref()
            .unwrap()
            .starts_with("Indirizzo non valido")
    );
    c.on_event(AppEvent::Discovery(found(
        "bbbb",
        "Ufficio",
        "192.168.1.3",
        PROTOCOL_VERSION,
    )));
    c.handle(Intent::ConnectHost {
        id: "bbbb".into(),
        pin: "  ".into(),
    });
    assert!(
        c.state().connect_error.as_deref().unwrap().contains("PIN"),
        "a host that does not know this PC needs the PIN"
    );
    c.handle(Intent::ConnectHost {
        id: "sparito".into(),
        pin: "1".into(),
    });
    assert!(
        c.state()
            .connect_error
            .as_deref()
            .unwrap()
            .contains("non è più in rete")
    );
    c.on_event(AppEvent::Discovery(found(
        "old",
        "Vecchio",
        "10.0.0.6",
        PROTOCOL_VERSION + 1,
    )));
    c.handle(Intent::ConnectHost {
        id: "old".into(),
        pin: "1".into(),
    });
    assert!(
        c.state()
            .connect_error
            .as_deref()
            .unwrap()
            .contains("Versione")
    );
    assert!(log.borrow().launched.is_empty());
    c.handle(Intent::SelectHost(None));
    assert_eq!(c.state().connect_error, None);
}

#[test]
fn close_to_tray_decision() {
    assert_eq!(close_decision(true), WindowCommand::HideToTray);
    assert_eq!(close_decision(false), WindowCommand::Quit);

    let (mut c, log) = controller();
    assert_eq!(c.handle(Intent::CloseWindow), Some(WindowCommand::Quit));
    c.handle(Intent::SetSharing(true));
    calls(&log);
    assert_eq!(
        c.handle(Intent::CloseWindow),
        Some(WindowCommand::HideToTray)
    );
    assert!(c.state().sharing.enabled, "sharing continues in the tray");
    assert!(calls(&log).is_empty());
}

#[test]
fn tray_actions() {
    let (mut c, log) = controller();
    assert_eq!(
        c.on_event(AppEvent::Tray(TrayAction::Open)),
        Some(WindowCommand::Show)
    );
    c.on_event(AppEvent::Tray(TrayAction::ToggleSharing));
    assert!(c.state().sharing.enabled);
    calls(&log);
    assert_eq!(
        c.on_event(AppEvent::Tray(TrayAction::Quit)),
        Some(WindowCommand::Quit)
    );
    assert!(!c.state().sharing.enabled);
    assert_eq!(calls(&log), ["stop"], "quitting stops sharing");
}

#[test]
fn cannot_switch_on_while_stopping() {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    c.handle(Intent::SetSharing(false));
    c.handle(Intent::SetSharing(true));
    c.on_event(AppEvent::Tray(TrayAction::ToggleSharing));
    assert_eq!(
        log.borrow().started.len(),
        1,
        "no second start while stopping"
    );
    assert!(!c.state().sharing.enabled);
    assert!(c.state().sharing.stopping);

    c.on_event(AppEvent::SharingStopped { generation: 1 });
    c.handle(Intent::ToggleSharing);
    assert!(c.state().sharing.enabled);
    assert_eq!(log.borrow().started[1].1, 2);
}

#[test]
fn late_events_of_the_stopping_run_are_ignored() {
    let (mut c, _log) = controller();
    c.handle(Intent::SetSharing(true));
    c.on_event(host(1, listening("111111", "ff")));
    c.handle(Intent::SetSharing(false));
    c.on_event(host(
        1,
        HostEvent::Stopped {
            error: Some(HostFailure::Failed("late".into())),
        },
    ));
    let sharing = &c.state().sharing;
    assert!(sharing.stopping);
    assert_eq!(sharing.error, None);
    assert_eq!(sharing.pin, None);
}

#[test]
fn quitting_while_stopping_waits_for_the_stop() {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    c.handle(Intent::SetSharing(false));
    calls(&log);
    assert_eq!(c.handle(Intent::Quit), Some(WindowCommand::Quit));
    assert_eq!(calls(&log), ["wait"]);
    assert!(!c.state().sharing.stopping);
    c.shutdown();
    assert!(calls(&log).is_empty(), "nothing left to wait for");
}

#[test]
fn closing_the_window_while_stopping_quits_after_the_stop() {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    c.handle(Intent::SetSharing(false));
    calls(&log);
    assert_eq!(c.handle(Intent::CloseWindow), Some(WindowCommand::Quit));
    assert_eq!(calls(&log), ["wait"]);
}
