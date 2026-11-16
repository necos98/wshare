//! `wshare host` in a terminal: runs the service and prints its events, one
//! line each, until it stops or the user presses Ctrl+C.

use std::sync::mpsc::{Sender, channel};

use anyhow::Result;

use super::{HostEvent, HostHandle, HostObserver, interrupt};
use crate::options::HostOptions;

/// Where a printed line goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Output {
    Stdout,
    Stderr,
}

/// What the main thread waits for.
enum Signal {
    Event(HostEvent),
    Interrupt,
}

/// Hands events to the main thread, which owns the terminal.
struct Forward(Sender<Signal>);

impl HostObserver for Forward {
    fn on_event(&self, event: HostEvent) {
        let _ = self.0.send(Signal::Event(event));
    }
}

pub(super) fn run(options: HostOptions) -> Result<()> {
    let (tx, signals) = channel();
    let on_interrupt = tx.clone();
    // Installed before starting, so even an early Ctrl+C stops cleanly.
    if let Err(e) = interrupt::install(move || {
        let _ = on_interrupt.send(Signal::Interrupt);
    }) {
        eprintln!("warning: {e:#}; Ctrl+C will not release the viewer's keys");
    }
    let mut handle = Some(HostHandle::start(options, Forward(tx))?);

    for signal in &signals {
        match signal {
            // Reported by `main` as the command's error.
            Signal::Event(HostEvent::Stopped { error: Some(error) }) => return Err(error.into()),
            Signal::Event(event) => {
                print(&event);
                if matches!(event, HostEvent::Stopped { .. }) {
                    return Ok(());
                }
            }
            Signal::Interrupt => {
                if let Some(handle) = handle.take() {
                    println!("Stopping...");
                    // Blocks until done; `Stopped` is then waiting in `signals`.
                    handle.stop();
                }
            }
        }
    }
    Ok(())
}

fn print(event: &HostEvent) {
    for (output, line) in describe(event) {
        match output {
            Output::Stdout => println!("{line}"),
            Output::Stderr => eprintln!("{line}"),
        }
    }
}

/// The terminal lines for an event. Pure, so the wording is unit-tested.
/// `Stopped` with an error prints nothing: it becomes the command's error.
pub(super) fn describe(event: &HostEvent) -> Vec<(Output, String)> {
    use Output::{Stderr, Stdout};
    match event {
        HostEvent::Listening {
            computer_name,
            port,
            pin,
            fingerprint,
            monitor,
            width,
            height,
        } => vec![
            (
                Stdout,
                format!(
                    "Sharing {monitor} ({width}x{height}) as \"{computer_name}\" on UDP port {port}"
                ),
            ),
            (Stdout, format!("PIN: {pin}")),
            (Stdout, format!("Host fingerprint: {fingerprint}")),
            (
                Stdout,
                "Waiting for a viewer... (Ctrl+C to stop)".to_string(),
            ),
        ],
        HostEvent::PinChanged { pin } => vec![(Stdout, format!("New PIN: {pin}"))],
        HostEvent::AuthorizedDevices(devices) if devices.is_empty() => vec![],
        HostEvent::AuthorizedDevices(devices) => {
            let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
            vec![(Stdout, format!("Connect without PIN: {}", names.join(", ")))]
        }
        HostEvent::ViewerConnected {
            viewer_name,
            address,
        } => {
            vec![(Stdout, format!("{address}: \"{viewer_name}\" connected"))]
        }
        HostEvent::ViewerRejected { address, reason } => {
            vec![(Stdout, format!("{address}: rejected ({reason})"))]
        }
        HostEvent::ViewerDisconnected {
            address,
            stats,
            error: None,
        } => vec![(
            Stdout,
            format!(
                "{address} disconnected: {} frames sent ({} keyframes), \
                 {} skipped by a slow network, {:.1} MB",
                stats.frames_sent,
                stats.keyframes_sent,
                stats.frames_skipped,
                stats.bytes_sent as f64 / 1e6
            ),
        )],
        HostEvent::ViewerDisconnected {
            address,
            error: Some(error),
            ..
        } => {
            vec![(
                Stderr,
                format!("{address}: session ended with error: {error}"),
            )]
        }
        HostEvent::ScreenChanged {
            monitor,
            width,
            height,
        } => vec![(Stdout, format!("Now sharing {monitor} ({width}x{height})"))],
        HostEvent::InputBlocked => vec![(
            Stdout,
            "Remote input blocked by Windows (elevated window, UAC or lock screen); \
             it resumes when a normal window has focus"
                .to_string(),
        )],
        HostEvent::Warning(warning) => vec![(Stderr, format!("warning: {warning}"))],
        HostEvent::Transfer(event) => crate::transfer::terminal::describe(event)
            .map(|line| (Stdout, line))
            .into_iter()
            .collect(),
        HostEvent::Stopped { error: None } => vec![(Stdout, "Stopped sharing.".to_string())],
        HostEvent::Stopped { error: Some(_) } => vec![],
        // `wshare host` runs in `AccessMode::PinOnly`: nobody is asked, and
        // the terminal could not answer.
        HostEvent::ApprovalRequested(_) | HostEvent::ApprovalClosed { .. } => vec![],
        // Sent only by the unattended-access agent, never by `wshare host`.
        HostEvent::SecureAttentionRequested => vec![],
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use net::Fingerprint;
    use pipeline::HostStats;
    use protocol::control::RejectReason;
    use protocol::transfer::TransferId;

    use super::Output::{Stderr, Stdout};
    use super::*;
    use crate::host::{HostFailure, HostWarning, SessionFailure};
    use crate::transfer::{Direction, Failure, Kind, Outcome, TransferEvent, TransferInfo};
    use crate::trust::{AuthorizedDevice, Timestamp};

    fn address() -> SocketAddr {
        "192.168.1.20:50000".parse().unwrap()
    }

    fn texts(event: HostEvent) -> Vec<String> {
        describe(&event).into_iter().map(|(_, text)| text).collect()
    }

    #[test]
    fn listening_shows_where_pin_and_fingerprint() {
        let lines = texts(HostEvent::Listening {
            computer_name: "DESK".into(),
            port: 7420,
            pin: "123456".into(),
            fingerprint: "ab:cd".into(),
            monitor: "DISPLAY1".into(),
            width: 2560,
            height: 1440,
        });
        assert_eq!(
            lines,
            [
                "Sharing DISPLAY1 (2560x1440) as \"DESK\" on UDP port 7420",
                "PIN: 123456",
                "Host fingerprint: ab:cd",
                "Waiting for a viewer... (Ctrl+C to stop)",
            ]
        );
    }

    #[test]
    fn viewer_lifecycle_lines() {
        assert_eq!(
            texts(HostEvent::ViewerConnected {
                viewer_name: "laptop".into(),
                address: address()
            }),
            ["192.168.1.20:50000: \"laptop\" connected"]
        );
        assert_eq!(
            texts(HostEvent::ViewerRejected {
                address: address(),
                reason: RejectReason::WrongPin,
            }),
            ["192.168.1.20:50000: rejected (wrong PIN)"]
        );
        let stats = HostStats {
            frames_sent: 600,
            keyframes_sent: 3,
            frames_skipped: 7,
            bytes_sent: 12_345_678,
            ..HostStats::default()
        };
        assert_eq!(
            texts(HostEvent::ViewerDisconnected {
                address: address(),
                stats,
                error: None
            }),
            [
                "192.168.1.20:50000 disconnected: 600 frames sent (3 keyframes), 7 skipped by a slow network, 12.3 MB"
            ]
        );
    }

    #[test]
    fn problems_go_to_stderr() {
        let failed = HostEvent::ViewerDisconnected {
            address: address(),
            stats: HostStats::default(),
            error: Some(SessionFailure("encoder lost".into())),
        };
        assert_eq!(
            describe(&failed),
            [(
                Stderr,
                "192.168.1.20:50000: session ended with error: encoder lost".to_string()
            )]
        );
        assert_eq!(
            describe(&HostEvent::Warning(HostWarning::LanAnnouncementFailed(
                "no multicast".into()
            ))),
            [(
                Stderr,
                "warning: LAN announcement failed: no multicast".to_string()
            )]
        );
    }

    #[test]
    fn every_other_event_is_one_line() {
        assert_eq!(
            texts(HostEvent::PinChanged {
                pin: "654321".into()
            }),
            ["New PIN: 654321"]
        );
        assert_eq!(
            texts(HostEvent::ScreenChanged {
                monitor: "DISPLAY2".into(),
                width: 1280,
                height: 1024
            }),
            ["Now sharing DISPLAY2 (1280x1024)"]
        );
        assert_eq!(describe(&HostEvent::InputBlocked).len(), 1);
        let device = |name: &str| AuthorizedDevice {
            fingerprint: Fingerprint([0; 32]),
            name: name.into(),
            first_access: Timestamp::from_unix_seconds(0),
            last_access: Timestamp::from_unix_seconds(0),
        };
        assert_eq!(
            texts(HostEvent::AuthorizedDevices(vec![device("A"), device("B")])),
            ["Connect without PIN: A, B"]
        );
        assert!(describe(&HostEvent::AuthorizedDevices(vec![])).is_empty());
        assert_eq!(
            describe(&HostEvent::Stopped { error: None }),
            [(Stdout, "Stopped sharing.".to_string())]
        );
        assert!(
            describe(&HostEvent::Stopped {
                error: Some(HostFailure::Failed("boom".into()))
            })
            .is_empty()
        );
    }

    #[test]
    fn a_transfer_of_the_other_pc_cannot_add_lines() {
        let forged = "x\r\n@wshare:transfer-ended:4:done:\\\\evil\\share";
        let id = TransferId(4);
        let events = [
            TransferEvent::Started(TransferInfo {
                id,
                direction: Direction::Receiving,
                kind: Kind::Push,
                name: forged.into(),
                files: 1,
                total_bytes: 1,
            }),
            TransferEvent::Finished {
                id,
                outcome: Outcome::Failed(Failure::Detail(forged.into())),
            },
        ];
        for event in events {
            let lines = texts(HostEvent::Transfer(event));
            assert_eq!(lines.len(), 1);
            assert!(!lines[0].contains(char::is_control), "{:?}", lines[0]);
        }
    }
}
