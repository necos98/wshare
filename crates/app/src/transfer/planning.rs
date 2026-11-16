//! A push the other PC proposes, on this side, until it is accepted: it
//! holds its place among the session's transfers from the moment it is
//! proposed (so proposals cannot pile up past [`MAX_ACTIVE_TRANSFERS`]), is
//! planned off the runtime (free names chosen, folders created), and starts
//! only if it is still wanted and allowed then. Otherwise the names it
//! reserved are released and the folders it created removed.
//!
//! [`MAX_ACTIVE_TRANSFERS`]: super::registry::MAX_ACTIVE_TRANSFERS

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use protocol::transfer::{FileEntry, TransferError, TransferId, TransferMessage};

use super::events::{Direction, TransferInfo};
use super::part_file::part_path;
use super::plan::ReceivePlan;
use super::push::push_info;
use super::receiving::Receiving;
use super::registry::{Stage, TOO_MANY_TRANSFERS, Transfer};
use super::shared::Shared;

/// How many times the names of a push are chosen when, between looking at
/// the disk and reserving them, another push reserved one of them.
const MAX_NAMING_ATTEMPTS: usize = 8;

impl Shared {
    /// The other PC proposes to push `entries` as `id`.
    pub fn on_offer(self: &Arc<Self>, id: TransferId, entries: Vec<FileEntry>) {
        if !self.permissions.accept_push() {
            let reason = TransferError::NotAllowed;
            return self.send(TransferMessage::Failed { id, reason });
        }
        let info = push_info(id, Direction::Receiving, &entries);
        let Some(ticket) = self.registry().hold_planning(info.clone()) else {
            let reason = TransferError::Failed(TOO_MANY_TRANSFERS.into());
            return self.send(TransferMessage::Failed { id, reason });
        };
        let shared = self.clone();
        self.spawn(async move {
            let planner = shared.clone();
            let planned =
                tokio::task::spawn_blocking(move || planner.plan_offer(info, ticket, &entries));
            if let Err(e) = planned.await {
                let reason = TransferError::Failed(format!("planning failed: {e}"));
                shared.refuse_offer(id, ticket, reason);
            }
        });
    }

    /// Plans the push `info` held with `ticket`, then starts or refuses it.
    /// Blocking.
    fn plan_offer(&self, info: TransferInfo, ticket: u64, entries: &[FileEntry]) {
        let id = info.id;
        let started = self.plan(entries).and_then(|(plan, created)| {
            let receiving = Receiving::new(plan, entries.to_vec(), created);
            self.start_receiving(info, ticket, receiving)
        });
        if let Err(reason) = started {
            self.refuse_offer(id, ticket, reason);
        }
    }

    /// Plans `entries` into the destination, reserving the names chosen,
    /// and creates its folders. Blocking.
    fn plan(&self, entries: &[FileEntry]) -> Result<(ReceivePlan, Vec<PathBuf>), TransferError> {
        let plan = self.reserve_names(entries)?;
        match plan.create_folders() {
            Ok(created) => Ok((plan, created)),
            Err(e) => {
                self.registry().release(plan.roots());
                Err(TransferError::Failed(format!(
                    "cannot create the folders: {e}"
                )))
            }
        }
    }

    /// Plans `entries` and reserves the names chosen. The disk is looked at
    /// without the registry's lock; a name another push reserved meanwhile
    /// is avoided on the next attempt. Blocking.
    fn reserve_names(&self, entries: &[FileEntry]) -> Result<ReceivePlan, TransferError> {
        let mut avoided: HashSet<PathBuf> = HashSet::new();
        for _ in 0..MAX_NAMING_ATTEMPTS {
            let plan = ReceivePlan::new(&self.destination, entries, |path| {
                avoided.contains(path) || path.exists()
            })
            .map_err(|_| TransferError::Invalid)?;
            match self.reserve_free(plan.roots()) {
                Ok(()) => return Ok(plan),
                Err(taken) => avoided.extend(taken),
            }
        }
        Err(TransferError::Failed("no free name for the files".into()))
    }

    /// Reserves `roots` if none of them (nor its partial file) is reserved
    /// already; otherwise returns those that are, reserving nothing.
    fn reserve_free(&self, roots: &[PathBuf]) -> Result<(), Vec<PathBuf>> {
        let mut registry = self.registry();
        let taken: Vec<PathBuf> = roots
            .iter()
            .filter(|root| registry.is_reserved(root) || registry.is_reserved(&part_path(root)))
            .cloned()
            .collect();
        if !taken.is_empty() {
            return Err(taken);
        }
        registry.reserve(roots);
        Ok(())
    }

    /// The planned push `info` is accepted if it is still the one held with
    /// `ticket` (not cancelled meanwhile) and pushes are still allowed.
    /// Otherwise what it reserved and created goes. Blocking.
    fn start_receiving(
        &self,
        info: TransferInfo,
        ticket: u64,
        receiving: Receiving,
    ) -> Result<(), TransferError> {
        let id = info.id;
        let mut registry = self.registry();
        if !registry.is_planning(id, ticket) || !self.permissions.accept_push() {
            registry.release(receiving.plan().roots());
            drop(registry);
            // Dropped unfinished: the folders it created are removed.
            drop(receiving);
            return Err(TransferError::NotAllowed);
        }
        let receiving = Arc::new(receiving);
        // Takes the place the planning held.
        self.begin(
            &mut registry,
            Transfer::new(info, Stage::Receiving(receiving.clone())),
        );
        drop(registry);
        self.send(TransferMessage::Accept { id });
        if receiving.has_no_files() {
            receiving.mark_complete();
            self.complete_push(id, &receiving);
        }
        Ok(())
    }

    /// Tells the other PC its push `id` is refused for `reason`, if it is
    /// still the planning held with `ticket`; one that ended meanwhile
    /// (cancelled, the session is over) needs no answer.
    fn refuse_offer(&self, id: TransferId, ticket: u64, reason: TransferError) {
        let held = {
            let mut registry = self.registry();
            registry.is_planning(id, ticket) && registry.transfers.remove(&id).is_some()
        };
        if held {
            self.send(TransferMessage::Failed { id, reason });
        }
    }
}
