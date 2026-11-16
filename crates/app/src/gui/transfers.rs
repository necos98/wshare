//! The file transfers the interface shows: those of the viewer connected to
//! this PC's sharing, and those of each viewer window it launched. Pure
//! state built from the transfer events.

use std::path::PathBuf;

use crate::text;
use crate::transfer::{ActiveTransfer, Direction, Ended, Outcome, TransferBoard, TransferEvent};

/// The transfers of one session, with how the last one ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTransfers {
    board: TransferBoard,
    /// How the last transfer ended, for a line under the list.
    pub last: Option<String>,
    /// Where files were received last: its folder can be opened.
    pub received: Option<PathBuf>,
}

impl FileTransfers {
    /// Takes `event` in; returns the transfer it ended, if any.
    pub fn apply(&mut self, event: TransferEvent) -> Option<Ended> {
        let ended = self.board.apply(event)?;
        if let (
            Direction::Receiving,
            Outcome::Completed {
                folder: Some(folder),
            },
        ) = (ended.info.direction, &ended.outcome)
        {
            self.received = Some(folder.clone());
        }
        self.last = Some(text::transfer_ended(&ended));
        Some(ended)
    }

    /// The session ended: nothing runs any more (what ended stays told).
    pub fn session_over(&mut self) {
        self.board.clear();
    }

    pub fn active(&self) -> &[ActiveTransfer] {
        self.board.active()
    }
}

#[cfg(test)]
mod tests {
    use protocol::transfer::TransferId;

    use super::*;
    use crate::transfer::{Kind, TransferInfo};

    fn started(direction: Direction) -> TransferEvent {
        TransferEvent::Started(TransferInfo {
            id: TransferId(1),
            direction,
            kind: Kind::Push,
            name: "Foto".into(),
            files: 1,
            total_bytes: 10,
        })
    }

    fn finished(outcome: Outcome) -> TransferEvent {
        TransferEvent::Finished {
            id: TransferId(1),
            outcome,
        }
    }

    #[test]
    fn a_received_transfer_leaves_its_folder_and_its_line() {
        let mut files = FileTransfers::default();
        files.apply(started(Direction::Receiving));
        assert_eq!(files.active().len(), 1);
        let folder = PathBuf::from("C:\\in");
        let ended = files.apply(finished(Outcome::Completed {
            folder: Some(folder.clone()),
        }));
        assert!(ended.is_some());
        assert!(files.active().is_empty());
        assert_eq!(files.received, Some(folder));
        assert_eq!(files.last.as_deref(), Some("Ricevuto «Foto»"));
    }

    #[test]
    fn a_failed_one_is_told_without_a_folder() {
        let mut files = FileTransfers::default();
        files.apply(started(Direction::Receiving));
        files.apply(finished(Outcome::Cancelled { by_peer: true }));
        assert_eq!(files.received, None);
        assert!(files.last.unwrap().contains("annullato"));
    }

    #[test]
    fn the_end_of_the_session_clears_what_runs() {
        let mut files = FileTransfers::default();
        files.apply(started(Direction::Sending));
        files.session_over();
        assert!(files.active().is_empty());
    }
}
