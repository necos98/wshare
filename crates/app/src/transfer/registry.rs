//! The bookkeeping of a session's transfers: which exist, what each waits
//! for, its progress and its cancel signal, the clipboard files this PC
//! offers, and the paths reserved by files still arriving. Pure state
//! behind the hub's lock; nothing here does I/O.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use protocol::transfer::{FileEntry, OfferId, TransferId};
use tokio::sync::{mpsc, watch};

use super::events::TransferInfo;
use super::listing::Listing;
use super::receiving::Receiving;

/// Most transfers a session runs at once; more are refused.
pub const MAX_ACTIVE_TRANSFERS: usize = 32;

/// Why a transfer is refused when [`MAX_ACTIVE_TRANSFERS`] are running.
pub const TOO_MANY_TRANSFERS: &str = "too many transfers at once";

/// Which side of the session this PC is: it decides the ids it may pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Host,
    Viewer,
}

impl Role {
    /// The host numbers its transfers 2, 4, 6...; the viewer 1, 3, 5...
    fn first_id(self) -> u32 {
        match self {
            Self::Host => 2,
            Self::Viewer => 1,
        }
    }

    /// Whether `id` is one the other side may pick.
    pub fn is_peer_id(self, id: TransferId) -> bool {
        id.0 % 2 != self.first_id() % 2
    }
}

/// A piece of a pasted file on its way to Explorer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    Data(Bytes),
    /// The whole file arrived. A channel closed without it means the
    /// transfer failed or was cancelled.
    End,
}

/// What a transfer waits for or does now.
pub enum Stage {
    /// The other PC proposed a push, which is being planned (names chosen,
    /// folders created) before it is accepted. It holds its place among
    /// the transfers of the session, but nothing was reported yet.
    /// `ticket` tells this planning from a later one under the same id.
    Planning { ticket: u64 },
    /// We proposed a push; the answer is awaited.
    Offered(Arc<Listing>),
    /// We are sending (a push the peer accepted, or a file it pasted).
    Sending,
    /// We accepted a push: its files are arriving.
    Receiving(Arc<Receiving>),
    /// We asked for a pasted file, listed as `entry`; `chunks` is `Some`
    /// until its stream arrives.
    Pulling {
        entry: FileEntry,
        chunks: Option<mpsc::Sender<Chunk>>,
    },
}

pub struct Transfer {
    pub info: TransferInfo,
    pub stage: Stage,
    pub progress: Arc<AtomicU64>,
    /// What the last progress event said.
    pub reported: u64,
    pub cancel: watch::Sender<bool>,
}

impl Transfer {
    pub fn new(info: TransferInfo, stage: Stage) -> Self {
        Self {
            info,
            stage,
            progress: Arc::new(AtomicU64::new(0)),
            reported: 0,
            cancel: watch::channel(false).0,
        }
    }
}

/// The clipboard files this PC offers now.
pub struct ClipboardOffer {
    pub id: OfferId,
    pub listing: Arc<Listing>,
}

pub struct Registry {
    role: Role,
    next_id: u32,
    next_offer: u32,
    pub clipboard: Option<ClipboardOffer>,
    /// Counts the changes of the clipboard: a list walked for an earlier
    /// copy is not offered once something else was copied.
    clipboard_generation: u64,
    pub transfers: HashMap<TransferId, Transfer>,
    reserved: HashSet<PathBuf>,
    next_ticket: u64,
}

impl Registry {
    pub fn new(role: Role) -> Self {
        Self {
            role,
            next_id: role.first_id(),
            next_offer: 1,
            clipboard: None,
            clipboard_generation: 0,
            transfers: HashMap::new(),
            reserved: HashSet::new(),
            next_ticket: 0,
        }
    }

    /// A fresh id of ours.
    pub fn allocate(&mut self) -> TransferId {
        let id = TransferId(self.next_id);
        self.next_id = self.next_id.wrapping_add(2);
        id
    }

    /// Whether the peer may start a transfer named `id` now: an id of its
    /// own, not in use, and room for one more.
    pub fn admits_peer(&self, id: TransferId) -> bool {
        self.role.is_peer_id(id) && !self.transfers.contains_key(&id) && !self.is_full()
    }

    pub fn is_full(&self) -> bool {
        self.transfers.len() >= MAX_ACTIVE_TRANSFERS
    }

    /// Holds a place for the peer's push `info` while it is planned, if
    /// [`Self::admits_peer`] its id; returns the planning's ticket.
    pub fn hold_planning(&mut self, info: TransferInfo) -> Option<u64> {
        if !self.admits_peer(info.id) {
            return None;
        }
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1);
        let id = info.id;
        self.transfers
            .insert(id, Transfer::new(info, Stage::Planning { ticket }));
        Some(ticket)
    }

    /// Whether `id` is still the planning held with `ticket` (not ended
    /// meanwhile, nor replaced).
    pub fn is_planning(&self, id: TransferId, ticket: u64) -> bool {
        matches!(
            self.transfers.get(&id).map(|t| &t.stage),
            Some(Stage::Planning { ticket: held }) if *held == ticket
        )
    }

    /// The clipboard changed: what it offered is gone. Returns the new
    /// generation, for [`Self::offer_clipboard_if`].
    pub fn withdraw_clipboard(&mut self) -> u64 {
        self.clipboard = None;
        self.clipboard_generation += 1;
        self.clipboard_generation
    }

    /// Offers `listing` if the clipboard did not change since `generation`.
    pub fn offer_clipboard_if(&mut self, generation: u64, listing: Listing) -> Option<OfferId> {
        (generation == self.clipboard_generation).then(|| self.offer_clipboard(listing))
    }

    /// Replaces the offered clipboard files; returns the new offer's id.
    pub fn offer_clipboard(&mut self, listing: Listing) -> OfferId {
        let id = OfferId(self.next_offer);
        self.next_offer = self.next_offer.wrapping_add(1);
        self.clipboard = Some(ClipboardOffer {
            id,
            listing: Arc::new(listing),
        });
        id
    }

    /// The file `index` of the clipboard files `offer`, while offered.
    pub fn clipboard_file(&self, offer: OfferId, index: u32) -> Option<(PathBuf, FileEntry)> {
        let current = self.clipboard.as_ref().filter(|c| c.id == offer)?;
        let (path, entry) = current.listing.file(index)?;
        Some((path.to_path_buf(), entry.clone()))
    }

    pub fn is_reserved(&self, path: &Path) -> bool {
        self.reserved.contains(path)
    }

    pub fn reserve(&mut self, paths: &[PathBuf]) {
        self.reserved.extend(paths.iter().cloned());
    }

    pub fn release(&mut self, paths: &[PathBuf]) {
        for path in paths {
            self.reserved.remove(path);
        }
    }

    /// Transfers whose progress moved since last reported, marked reported.
    pub fn moved(&mut self) -> Vec<(TransferId, u64)> {
        self.transfers
            .iter_mut()
            .filter_map(|(id, transfer)| {
                let done = transfer.progress.load(Ordering::Relaxed);
                (done != transfer.reported).then(|| {
                    transfer.reported = done;
                    (*id, done)
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::events::{Direction, Kind};
    use super::*;

    fn info(id: TransferId) -> TransferInfo {
        TransferInfo {
            id,
            direction: Direction::Sending,
            kind: Kind::Push,
            name: "a".into(),
            files: 1,
            total_bytes: 1,
        }
    }

    #[test]
    fn the_two_sides_never_pick_the_same_id() {
        let mut host = Registry::new(Role::Host);
        let mut viewer = Registry::new(Role::Viewer);
        let host_ids: Vec<u32> = (0..3).map(|_| host.allocate().0).collect();
        let viewer_ids: Vec<u32> = (0..3).map(|_| viewer.allocate().0).collect();
        assert_eq!(host_ids, [2, 4, 6]);
        assert_eq!(viewer_ids, [1, 3, 5]);
        assert!(host.admits_peer(TransferId(1)));
        assert!(!host.admits_peer(TransferId(2)), "one of ours");
        assert!(viewer.admits_peer(TransferId(2)));
        assert!(!viewer.admits_peer(TransferId(3)));
    }

    #[test]
    fn ids_in_use_and_a_full_session_are_refused() {
        let mut host = Registry::new(Role::Host);
        host.transfers.insert(
            TransferId(1),
            Transfer::new(info(TransferId(1)), Stage::Sending),
        );
        assert!(!host.admits_peer(TransferId(1)));
        for n in 0..MAX_ACTIVE_TRANSFERS as u32 {
            let id = TransferId(101 + 2 * n);
            host.transfers
                .insert(id, Transfer::new(info(id), Stage::Sending));
        }
        assert!(!host.admits_peer(TransferId(9999)));
    }

    #[test]
    fn pushes_being_planned_count_against_the_limit() {
        let mut host = Registry::new(Role::Host);
        let tickets: Vec<u64> = (0..MAX_ACTIVE_TRANSFERS as u32)
            .map(|n| host.hold_planning(info(TransferId(1 + 2 * n))).unwrap())
            .collect();
        assert!(host.is_full());
        assert_eq!(host.hold_planning(info(TransferId(999))), None);
        assert!(host.is_planning(TransferId(1), tickets[0]));
        assert!(
            !host.is_planning(TransferId(1), tickets[1]),
            "another ticket"
        );
        host.transfers.remove(&TransferId(1));
        assert!(!host.is_planning(TransferId(1), tickets[0]), "ended");
        let again = host.hold_planning(info(TransferId(1))).unwrap();
        assert!(!host.is_planning(TransferId(1), tickets[0]), "replaced");
        assert!(host.is_planning(TransferId(1), again));
    }

    #[test]
    fn only_the_current_clipboard_offer_is_served() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let listing = super::super::listing::list(&[dir.path().join("a.txt")]).unwrap();
        let mut registry = Registry::new(Role::Host);
        let first = registry.offer_clipboard(listing.clone());
        assert!(registry.clipboard_file(first, 0).is_some());
        assert!(registry.clipboard_file(first, 1).is_none());
        let second = registry.offer_clipboard(listing);
        assert_ne!(first, second);
        assert!(registry.clipboard_file(first, 0).is_none(), "replaced");
        assert!(registry.clipboard_file(second, 0).is_some());
    }

    #[test]
    fn progress_is_reported_only_when_it_moved() {
        let mut registry = Registry::new(Role::Viewer);
        let transfer = Transfer::new(info(TransferId(1)), Stage::Sending);
        let progress = transfer.progress.clone();
        registry.transfers.insert(TransferId(1), transfer);
        assert!(registry.moved().is_empty());
        progress.store(10, Ordering::Relaxed);
        assert_eq!(registry.moved(), [(TransferId(1), 10)]);
        assert!(registry.moved().is_empty());
    }
}
