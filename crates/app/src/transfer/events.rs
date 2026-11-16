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
    Completed { folder: Option<PathBuf> },
    /// Cancelled on this PC (`by_peer: false`) or on the other one.
    Cancelled { by_peer: bool },
    /// The other PC refused the files.
    Refused(TransferError),
    /// The transfer stopped for `Failure`, after it began or before it
    /// could be proposed.
    Failed(Failure),
    /// The session ended before the transfer did.
    Interrupted,
}

/// Why a transfer failed. The known causes are typed (their words are in
/// `crate::text`); only the technical detail of an I/O error stays text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// A cause the two PCs know: this one's, or the other PC's. Never
    /// `TransferError::Failed`, whose text is [`Self::Detail`] (see
    /// [`Self::from_error`]).
    Cause(TransferError),
    /// The permission to receive was taken back while it ran.
    NoLongerAllowed,
    /// The technical detail of what broke (an I/O error...).
    Detail(String),
}

impl Failure {
    /// The failure a reason of the other PC (or of this one) stands for.
    pub fn from_error(error: TransferError) -> Self {
        match error {
            TransferError::Failed(detail) => Self::Detail(detail),
            known => Self::Cause(known),
        }
    }
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
