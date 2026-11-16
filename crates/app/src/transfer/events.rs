//! What a session's transfers report to the front-end: one `Started`, then
//! `Progress` now and then, then one `Finished`, per transfer.

use std::path::PathBuf;

use protocol::transfer::{TransferError, TransferId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Sending,
    Receiving,
}

/// How the files cross.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Dropped on the window or picked with "Invia file…".
    Push,
    /// Copied on one PC and pasted on the other (one file per transfer).
    Paste,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferInfo {
    pub id: TransferId,
    pub direction: Direction,
    pub kind: Kind,
    /// The first item's name (the others are counted in `files`).
    pub name: String,
    pub files: usize,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Everything arrived. `folder` is where, when this PC received the
    /// files into a folder it chose (not for a paste: Explorer knows).
    Completed {
        folder: Option<PathBuf>,
    },
    /// Cancelled on this PC (`by_peer: false`) or on the other one.
    Cancelled {
        by_peer: bool,
    },
    /// The other PC refused the files.
    Refused(TransferError),
    Failed(String),
    /// The session ended before the transfer did.
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferEvent {
    Started(TransferInfo),
    Progress { id: TransferId, done_bytes: u64 },
    Finished { id: TransferId, outcome: Outcome },
}

/// Receives the transfer events of a session. Called from the session's
/// tasks: implementations must be quick and thread-safe.
pub trait TransferObserver: Send + Sync + 'static {
    fn on_event(&self, event: TransferEvent);
}

impl<F: Fn(TransferEvent) + Send + Sync + 'static> TransferObserver for F {
    fn on_event(&self, event: TransferEvent) {
        self(event);
    }
}

/// What the user can ask of the transfers of the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferCommand {
    /// Push these files and folders to the other PC.
    Send(Vec<PathBuf>),
    Cancel(TransferId),
    CancelAll,
}
