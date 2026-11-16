//! Controller behaviour around files: the dialogs, sending to the viewer
//! connected here or from a viewer window, cancelling, the folder of
//! received files and the notification of the viewer's files.

use std::path::PathBuf;

use protocol::transfer::TransferId;

use super::fakes::{Shared, TestController, calls, controller, host};
use super::*;
use crate::gui::model::tests::listening;
use crate::gui::ports::ChoicePurpose;
use crate::transfer::{Direction, Kind, Outcome, TransferCommand, TransferEvent, TransferInfo};

fn sharing_with_viewer() -> (TestController, Shared) {
    let (mut c, log) = controller();
    c.handle(Intent::SetSharing(true));
    c.on_event(host(1, listening("123456", "ab")));
    c.on_event(host(
        1,
        HostEvent::ViewerConnected {
            viewer_name: "PORTATILE".into(),
            address: "192.168.1.7:5000".parse().unwrap(),
        },
    ));
    calls(&log);
    // The balloon of the connection is not what these tests look at.
    c.take_notifications();
    (c, log)
}

fn paths(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

fn received(id: u32) -> [TransferEvent; 2] {
    [
        TransferEvent::Started(TransferInfo {
            id: TransferId(id),
            direction: Direction::Receiving,
            kind: Kind::Push,
            name: "Foto".into(),
            files: 2,
            total_bytes: 10,
        }),
        TransferEvent::Finished {
            id: TransferId(id),
            outcome: Outcome::Completed {
                folder: Some(PathBuf::from("C:\\in")),
            },
        },
    ]
}

#[test]
fn files_chosen_for_the_viewer_go_to_the_service() {
    let (mut c, log) = sharing_with_viewer();
    c.handle(Intent::SendToViewer);
    assert_eq!(log.borrow().chosen, [ChoicePurpose::SendToViewer]);
    c.on_event(AppEvent::Chosen {
        purpose: ChoicePurpose::SendToViewer,
        paths: paths(&["C:\\a.txt", "C:\\Foto"]),
    });
    assert_eq!(log.borrow().sent, [paths(&["C:\\a.txt", "C:\\Foto"])]);
    assert_eq!(c.state().files_notice, None);
}

#[test]
fn a_cancelled_dialog_sends_nothing() {
    let (mut c, log) = sharing_with_viewer();
    c.on_event(AppEvent::Chosen {
        purpose: ChoicePurpose::SendToViewer,
        paths: Vec::new(),
    });
    assert!(log.borrow().sent.is_empty());
}

#[test]
fn files_for_a_viewer_that_left_are_told_about() {
    let (mut c, log) = sharing_with_viewer();
    log.borrow_mut().no_viewer = true;
    c.on_event(AppEvent::Chosen {
        purpose: ChoicePurpose::SendToViewer,
        paths: paths(&["C:\\a.txt"]),
    });
    assert!(c.state().files_notice.is_some());
}

#[test]
fn a_viewer_window_gets_its_files_and_cancellations_as_commands() {
    let (mut c, log) = controller();
    c.handle(Intent::SendFromViewer(4));
    assert_eq!(log.borrow().chosen, [ChoicePurpose::SendFromViewer(4)]);
    c.on_event(AppEvent::Chosen {
        purpose: ChoicePurpose::SendFromViewer(4),
        paths: paths(&["D:\\x"]),
    });
    c.handle(Intent::CancelViewerTransfer {
        viewer: 4,
        transfer: Some(TransferId(3)),
    });
    c.handle(Intent::CancelViewerTransfer {
        viewer: 4,
        transfer: None,
    });
    assert_eq!(
        log.borrow().commands,
        [
            (4, TransferCommand::Send(paths(&["D:\\x"]))),
            (4, TransferCommand::Cancel(TransferId(3))),
            (4, TransferCommand::CancelAll),
        ]
    );

    log.borrow_mut().fail_command = true;
    c.handle(Intent::CancelViewerTransfer {
        viewer: 4,
        transfer: None,
    });
    assert!(
        c.state().files_notice.is_some(),
        "a viewer not reached is told"
    );
}

#[test]
fn cancelling_a_transfer_of_the_viewer_here_reaches_the_service() {
    let (mut c, log) = sharing_with_viewer();
    c.handle(Intent::CancelHostTransfer(Some(TransferId(2))));
    c.handle(Intent::CancelHostTransfer(None));
    assert_eq!(calls(&log), ["cancel_transfer 2", "cancel_transfer all"]);
}

#[test]
fn the_folder_of_received_files_opens_and_a_failure_is_told() {
    let (mut c, log) = controller();
    c.handle(Intent::OpenFolder(PathBuf::from("C:\\in")));
    assert_eq!(log.borrow().opened, [PathBuf::from("C:\\in")]);
    log.borrow_mut().fail_open = true;
    c.handle(Intent::OpenFolder(PathBuf::from("C:\\in")));
    assert!(
        c.state()
            .files_notice
            .as_ref()
            .unwrap()
            .contains("access denied")
    );
}

#[test]
fn the_download_folder_chosen_is_saved() {
    let (mut c, log) = controller();
    c.handle(Intent::ChooseDownloads);
    assert_eq!(log.borrow().chosen, [ChoicePurpose::Downloads]);
    c.on_event(AppEvent::Chosen {
        purpose: ChoicePurpose::Downloads,
        paths: paths(&["D:\\Ricevuti"]),
    });
    assert_eq!(c.state().settings.downloads, "D:\\Ricevuti");
    assert_eq!(log.borrow().saved.last().unwrap().downloads, "D:\\Ricevuti");
}

#[test]
fn the_reception_setting_is_live() {
    let (mut c, log) = sharing_with_viewer();
    let mut settings = c.state().settings.clone();
    settings.receive_files = false;
    c.handle(Intent::ChangeSettings(settings));
    assert_eq!(calls(&log), ["receive_files false"]);
    assert!(!c.state().restart_needed());
}

#[test]
fn a_new_download_folder_needs_a_restart_of_the_sharing() {
    let (mut c, _log) = sharing_with_viewer();
    let mut settings = c.state().settings.clone();
    settings.downloads = "D:\\altrove".into();
    c.handle(Intent::ChangeSettings(settings));
    assert!(c.state().restart_needed());
}

#[test]
fn files_received_from_the_viewer_are_shown_and_announced() {
    let (mut c, _log) = sharing_with_viewer();
    for event in received(1) {
        c.on_event(host(1, HostEvent::Transfer(event)));
    }
    let files = &c.state().sharing.files;
    assert!(files.active().is_empty());
    assert_eq!(files.received, Some(PathBuf::from("C:\\in")));
    let notifications = c.take_notifications();
    assert_eq!(notifications.len(), 1);
    assert!(
        notifications[0].body.contains("Foto"),
        "{:?}",
        notifications[0]
    );
    assert!(notifications[0].body.contains("PORTATILE"));
}

#[test]
fn a_viewer_windows_transfers_follow_its_lines() {
    let (mut c, _log) = controller();
    c.handle(Intent::ConnectAddress {
        address: "192.168.1.9".into(),
        pin: "123456".into(),
    });
    let id = c.state().viewers[0].id;
    c.on_event(AppEvent::Viewer {
        id,
        update: ViewerUpdate::Connected,
    });
    let [started, finished] = received(5);
    c.on_event(AppEvent::Viewer {
        id,
        update: ViewerUpdate::Transfer(started),
    });
    assert_eq!(c.state().viewers[0].files.active().len(), 1);
    c.on_event(AppEvent::Viewer {
        id,
        update: ViewerUpdate::Transfer(finished),
    });
    assert!(c.state().viewers[0].files.active().is_empty());
    assert_eq!(
        c.state().viewers[0].files.received,
        Some(PathBuf::from("C:\\in"))
    );
}
