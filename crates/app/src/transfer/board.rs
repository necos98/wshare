//! The transfers in progress as a front-end shows them: built from the
//! events alone, so the viewer window and the desktop interface read the
//! same state. Pure.

use protocol::transfer::TransferId;

use super::events::{Direction, Outcome, TransferEvent, TransferInfo};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTransfer {
    pub info: TransferInfo,
    pub done_bytes: u64,
}

impl ActiveTransfer {
    /// How far along, 0 to 100.
    pub fn percent(&self) -> u8 {
        percent(self.done_bytes, self.info.total_bytes)
    }
}

/// All the transfers in progress, folded into one line's worth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub transfers: usize,
    /// The first transfer's name, when there is only one.
    pub name: Option<String>,
    pub files: usize,
    /// `None` when files go both ways.
    pub direction: Option<Direction>,
    pub done_bytes: u64,
    pub total_bytes: u64,
}

impl Summary {
    pub fn percent(&self) -> u8 {
        percent(self.done_bytes, self.total_bytes)
    }
}

/// A transfer that just ended, for a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ended {
    pub info: TransferInfo,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferBoard {
    active: Vec<ActiveTransfer>,
}

impl TransferBoard {
    /// Takes `event` in; returns the transfer it ended, if it ended one.
    pub fn apply(&mut self, event: TransferEvent) -> Option<Ended> {
        match event {
            TransferEvent::Started(info) => {
                self.active.retain(|a| a.info.id != info.id);
                self.active.push(ActiveTransfer {
                    info,
                    done_bytes: 0,
                });
                None
            }
            TransferEvent::Progress { id, done_bytes } => {
                if let Some(active) = self.find_mut(id) {
                    active.done_bytes = done_bytes.min(active.info.total_bytes);
                }
                None
            }
            TransferEvent::Finished { id, outcome } => {
                let index = self.active.iter().position(|a| a.info.id == id)?;
                let ended = self.active.remove(index);
                Some(Ended {
                    info: ended.info,
                    outcome,
                })
            }
        }
    }

    /// Every transfer ended (the session is over).
    pub fn clear(&mut self) {
        self.active.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    pub fn active(&self) -> &[ActiveTransfer] {
        &self.active
    }

    pub fn summary(&self) -> Option<Summary> {
        let first = self.active.first()?;
        let same_way = self
            .active
            .iter()
            .all(|a| a.info.direction == first.info.direction);
        Some(Summary {
            transfers: self.active.len(),
            name: (self.active.len() == 1).then(|| first.info.name.clone()),
            files: self.active.iter().map(|a| a.info.files).sum(),
            direction: same_way.then_some(first.info.direction),
            done_bytes: self.active.iter().map(|a| a.done_bytes).sum(),
            total_bytes: self.active.iter().map(|a| a.info.total_bytes).sum(),
        })
    }

    fn find_mut(&mut self, id: TransferId) -> Option<&mut ActiveTransfer> {
        self.active.iter_mut().find(|a| a.info.id == id)
    }
}

/// `done` of `total` as 0 to 100; an empty transfer is done when it ends.
fn percent(done: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    let percent = u128::from(done.min(total)) * 100 / u128::from(total);
    u8::try_from(percent).unwrap_or(100)
}

#[cfg(test)]
mod tests {
    use super::super::events::Kind;
    use super::*;

    fn info(id: u32, direction: Direction, total_bytes: u64) -> TransferInfo {
        TransferInfo {
            id: TransferId(id),
            direction,
            kind: Kind::Push,
            name: format!("file{id}"),
            files: 2,
            total_bytes,
        }
    }

    #[test]
    fn progress_and_end_of_one_transfer() {
        let mut board = TransferBoard::default();
        assert_eq!(board.summary(), None);
        board.apply(TransferEvent::Started(info(1, Direction::Receiving, 200)));
        board.apply(TransferEvent::Progress {
            id: TransferId(1),
            done_bytes: 50,
        });
        let summary = board.summary().unwrap();
        assert_eq!(summary.percent(), 25);
        assert_eq!(summary.name.as_deref(), Some("file1"));
        assert_eq!(summary.direction, Some(Direction::Receiving));

        let ended = board.apply(TransferEvent::Finished {
            id: TransferId(1),
            outcome: Outcome::Cancelled { by_peer: true },
        });
        assert_eq!(ended.unwrap().info.name, "file1");
        assert!(board.is_empty());
    }

    #[test]
    fn several_transfers_add_up() {
        let mut board = TransferBoard::default();
        board.apply(TransferEvent::Started(info(1, Direction::Receiving, 100)));
        board.apply(TransferEvent::Started(info(2, Direction::Sending, 300)));
        board.apply(TransferEvent::Progress {
            id: TransferId(2),
            done_bytes: 100,
        });
        let summary = board.summary().unwrap();
        assert_eq!(summary.transfers, 2);
        assert_eq!(summary.name, None);
        assert_eq!(summary.files, 4);
        assert_eq!(summary.direction, None, "both ways");
        assert_eq!(summary.percent(), 25);
    }

    #[test]
    fn unknown_ids_and_overshoot_are_harmless() {
        let mut board = TransferBoard::default();
        assert_eq!(
            board.apply(TransferEvent::Finished {
                id: TransferId(9),
                outcome: Outcome::Failed("x".into()),
            }),
            None
        );
        board.apply(TransferEvent::Started(info(1, Direction::Sending, 10)));
        board.apply(TransferEvent::Progress {
            id: TransferId(1),
            done_bytes: 99,
        });
        assert_eq!(board.active()[0].percent(), 100);
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(u64::MAX, u64::MAX), 100);
    }
}
