//! The terminal's line for a transfer event (`wshare host`, `wshare view`):
//! English, like the rest of the command line. Progress is not printed.

use super::events::{Direction, Kind, Outcome, TransferEvent, TransferInfo};
use super::shared::describe as reason_text;

/// Bytes in a megabyte, as the terminal counts them.
const MEGABYTE: f64 = 1e6;

pub fn describe(event: &TransferEvent) -> Option<String> {
    match event {
        TransferEvent::Started(info) => Some(started(info)),
        TransferEvent::Progress { .. } => None,
        TransferEvent::Finished { id, outcome } => {
            let id = id.0;
            Some(match outcome {
                Outcome::Completed {
                    folder: Some(folder),
                } => {
                    format!("Transfer {id}: done, in {}", folder.display())
                }
                Outcome::Completed { folder: None } => format!("Transfer {id}: done"),
                Outcome::Cancelled { by_peer: false } => format!("Transfer {id}: cancelled"),
                Outcome::Cancelled { by_peer: true } => {
                    format!("Transfer {id}: cancelled on the other computer")
                }
                Outcome::Refused(reason) => {
                    format!("Transfer {id}: refused ({})", reason_text(reason))
                }
                Outcome::Failed(detail) => format!("Transfer {id}: failed ({detail})"),
                Outcome::Interrupted => format!("Transfer {id}: interrupted, the session ended"),
            })
        }
    }
}

fn started(info: &TransferInfo) -> String {
    let verb = match (info.direction, info.kind) {
        (Direction::Sending, Kind::Push) => "sending",
        (Direction::Receiving, Kind::Push) => "receiving",
        (Direction::Sending, Kind::Paste) => "pasted on the other computer",
        (Direction::Receiving, Kind::Paste) => "pasting",
    };
    format!(
        "Transfer {}: {verb} \"{}\" ({} file(s), {:.1} MB)",
        info.id.0,
        info.name,
        info.files,
        info.total_bytes as f64 / MEGABYTE
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use protocol::transfer::{TransferError, TransferId};

    use super::*;

    #[test]
    fn a_transfer_is_told_when_it_starts_and_ends_only() {
        let info = TransferInfo {
            id: TransferId(3),
            direction: Direction::Receiving,
            kind: Kind::Push,
            name: "Foto".into(),
            files: 2,
            total_bytes: 2_500_000,
        };
        assert_eq!(
            describe(&TransferEvent::Started(info)).unwrap(),
            "Transfer 3: receiving \"Foto\" (2 file(s), 2.5 MB)"
        );
        let id = TransferId(3);
        assert_eq!(
            describe(&TransferEvent::Progress { id, done_bytes: 1 }),
            None
        );
        let ended = |outcome| describe(&TransferEvent::Finished { id, outcome }).unwrap();
        assert_eq!(
            ended(Outcome::Completed {
                folder: Some(PathBuf::from("C:\\in"))
            }),
            "Transfer 3: done, in C:\\in"
        );
        assert!(ended(Outcome::Refused(TransferError::NotAllowed)).contains("not allowed"));
        assert!(ended(Outcome::Interrupted).contains("interrupted"));
    }
}
