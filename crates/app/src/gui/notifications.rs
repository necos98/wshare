//! What is worth a balloon in the notification area: a viewer that
//! asks to connect, connects, leaves, or is refused. Pure decisions; `tray`
//! shows them.

use crate::host::HostEvent;
use crate::text;
use crate::transfer::{Ended, Kind};

use super::model::ConnectedViewer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

/// The balloon for a pushed transfer of the connected viewer that ended
/// (`viewer` is its name). Pasted files are not announced: Explorer shows
/// their copy itself.
pub fn for_transfer(ended: &Ended, viewer: Option<&str>) -> Option<Notification> {
    (ended.info.kind == Kind::Push).then(|| Notification {
        title: text::NOTIFY_TITLE.to_string(),
        body: text::transfer_notification(ended, viewer),
    })
}

/// The balloon for `event`, if it deserves one. `viewer` is the one
/// connected before the event, which names who left.
pub fn for_host_event(event: &HostEvent, viewer: Option<&ConnectedViewer>) -> Option<Notification> {
    let body = match event {
        HostEvent::ViewerConnected { viewer_name, .. } => text::notify_connected(viewer_name),
        HostEvent::ViewerDisconnected { .. } => {
            text::notify_disconnected(viewer.map(|v| v.name.as_str()))
        }
        HostEvent::ViewerRejected { address, reason } => {
            text::viewer_rejected(address.ip(), reason)
        }
        HostEvent::ApprovalRequested(request) => {
            text::approval_message(&request.viewer_name, request.address.ip())
        }
        _ => return None,
    };
    Some(Notification {
        title: text::NOTIFY_TITLE.to_string(),
        body,
    })
}

#[cfg(test)]
mod tests {
    use pipeline::HostStats;
    use protocol::control::RejectReason;

    use super::*;

    fn address() -> std::net::SocketAddr {
        "192.168.1.7:5000".parse().unwrap()
    }

    fn viewer(name: &str) -> ConnectedViewer {
        ConnectedViewer {
            name: name.into(),
            address: address(),
        }
    }

    #[test]
    fn pushed_files_are_announced_and_pasted_ones_are_not() {
        use protocol::transfer::TransferId;

        use crate::transfer::{Direction, Outcome, TransferInfo};

        let ended = |kind| Ended {
            info: TransferInfo {
                id: TransferId(1),
                direction: Direction::Receiving,
                kind,
                name: "Foto".into(),
                files: 2,
                total_bytes: 9,
            },
            outcome: Outcome::Completed {
                folder: Some(std::path::PathBuf::from("C:/in")),
            },
        };
        let notification = for_transfer(&ended(Kind::Push), Some("PORTATILE")).unwrap();
        assert_eq!(
            notification.body,
            "Ricevuto «Foto» (PORTATILE). Salvati in C:/in"
        );
        assert_eq!(for_transfer(&ended(Kind::Paste), None), None);
    }

    #[test]
    fn a_connecting_viewer_is_announced_by_name() {
        let event = HostEvent::ViewerConnected {
            viewer_name: "PORTATILE".into(),
            address: address(),
        };
        let notification = for_host_event(&event, None).unwrap();
        assert_eq!(notification.title, "wshare");
        assert_eq!(notification.body, "PORTATILE si è connesso");
    }

    #[test]
    fn a_leaving_viewer_is_named_from_the_state_before() {
        let event = HostEvent::ViewerDisconnected {
            address: address(),
            stats: HostStats::default(),
            error: None,
        };
        assert_eq!(
            for_host_event(&event, Some(&viewer("PORTATILE")))
                .unwrap()
                .body,
            "PORTATILE si è disconnesso"
        );
        assert_eq!(
            for_host_event(&event, None).unwrap().body,
            "Il visualizzatore si è disconnesso"
        );
    }

    #[test]
    fn a_refused_viewer_says_why() {
        let event = HostEvent::ViewerRejected {
            address: address(),
            reason: RejectReason::Busy,
        };
        let body = for_host_event(&event, None).unwrap().body;
        assert!(body.contains("192.168.1.7"), "{body}");
    }

    #[test]
    fn a_failed_admission_is_worded_in_italian_in_the_bubble() {
        let event = HostEvent::ViewerRejected {
            address: address(),
            reason: RejectReason::NoHello,
        };
        let body = for_host_event(&event, None).unwrap().body;
        assert!(body.contains("non si è presentato in tempo"), "{body}");
        assert!(!body.contains("Hello"), "{body}");
    }

    #[test]
    fn other_events_stay_quiet() {
        for event in [
            HostEvent::InputBlocked,
            HostEvent::PinChanged {
                pin: "123456".into(),
            },
            HostEvent::Stopped { error: None },
        ] {
            assert_eq!(for_host_event(&event, None), None, "{event:?}");
        }
    }
}
