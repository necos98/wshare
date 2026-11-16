//! What is worth a balloon in the notification area: a viewer that
//! asks to connect, connects, leaves, or is refused. Pure decisions; `tray`
//! shows them.

use crate::host::HostEvent;
use crate::text;

use super::model::ConnectedViewer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
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
