//! Pushed transfers (drag and drop, "Invia file…"): proposing a list and
//! sending it once accepted, and on the other side receiving its files
//! (see [`super::planning`] for how it is planned and accepted).

use std::path::PathBuf;
use std::sync::Arc;

use net::FileReceiver;
use protocol::transfer::{FileEntry, StreamHeader, TransferError, TransferId, TransferMessage};

use super::events::{Direction, Failure, Kind, Outcome, TransferInfo};
use super::listing::{self, Listing};
use super::part_file::{PartFile, blocking};
use super::plan::{Placed, total_size};
use super::receiving::Receiving;
use super::registry::{Stage, Transfer};
use super::shared::Shared;
use super::streams::{StreamError, receive_file, send_file};

/// The name a list is shown with: its first item.
fn first_name(entries: &[FileEntry]) -> String {
    entries
        .first()
        .map(|e| e.path.split('/').next().unwrap_or_default().to_string())
        .unwrap_or_default()
}

/// How a push of `entries` is shown. The sizes may come from the other PC,
/// unchecked yet: a total beyond `u64::MAX` shows as `u64::MAX`.
pub(super) fn push_info(
    id: TransferId,
    direction: Direction,
    entries: &[FileEntry],
) -> TransferInfo {
    TransferInfo {
        id,
        direction,
        kind: Kind::Push,
        name: first_name(entries),
        files: entries.iter().filter(|e| !e.is_dir).count(),
        total_bytes: total_size(entries).unwrap_or(u64::MAX),
    }
}

/// The name shown for a list that could not be made: the first item picked.
fn picked_name(picked: &[PathBuf]) -> String {
    picked
        .first()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl Shared {
    /// The user wants to push `picked`: lists it and proposes it.
    pub fn start_push(self: &Arc<Self>, picked: Vec<PathBuf>) {
        let id = self.registry().allocate();
        // What is shown if the list cannot even be proposed.
        let unsent = TransferInfo {
            id,
            direction: Direction::Sending,
            kind: Kind::Push,
            name: picked_name(&picked),
            files: picked.len(),
            total_bytes: 0,
        };
        if !self.permissions.may_send() {
            return self.stillborn(unsent, Outcome::Refused(TransferError::NotAllowed));
        }
        let shared = self.clone();
        self.spawn(async move {
            let listing = blocking(move || {
                listing::list(&picked).map_err(|e| std::io::Error::other(e.to_string()))
            })
            .await;
            let listing = match listing {
                Ok(listing) => listing,
                Err(e) => {
                    return shared
                        .stillborn(unsent, Outcome::Failed(Failure::Detail(e.to_string())));
                }
            };
            let mut registry = shared.registry();
            if registry.is_full() {
                drop(registry);
                let outcome = Outcome::Failed(Failure::Cause(TransferError::TooManyTransfers));
                return shared.stillborn(unsent, outcome);
            }
            let entries = listing.entries.clone();
            let info = push_info(id, Direction::Sending, &entries);
            let transfer = Transfer::new(info, Stage::Offered(Arc::new(listing)));
            shared.begin(&mut registry, transfer);
            drop(registry);
            shared.send(TransferMessage::Offer { id, entries });
        });
    }

    /// The other PC accepted our push `id`: its files go.
    pub fn on_accept(self: &Arc<Self>, id: TransferId) {
        let mut registry = self.registry();
        let Some(transfer) = registry.transfers.get_mut(&id) else {
            return;
        };
        let Stage::Offered(listing) = &transfer.stage else {
            return;
        };
        let listing = listing.clone();
        transfer.stage = Stage::Sending;
        let cancel = transfer.cancel.subscribe();
        let progress = transfer.progress.clone();
        drop(registry);
        let shared = self.clone();
        self.spawn(async move {
            if let Err(error) = shared.send_listing(id, &listing, cancel, &progress).await {
                shared.fail(id, error);
            }
            // Done once the other PC says `Completed`.
        });
    }

    async fn send_listing(
        &self,
        id: TransferId,
        listing: &Listing,
        cancel: tokio::sync::watch::Receiver<bool>,
        progress: &std::sync::atomic::AtomicU64,
    ) -> Result<(), StreamError> {
        for index in 0..listing.entries.len() {
            let index = u32::try_from(index).map_err(|_| StreamError::Cancelled)?;
            let Some((source, entry)) = listing.file(index) else {
                continue;
            };
            let header = StreamHeader {
                id,
                index,
                entry: entry.clone(),
            };
            send_file(&self.files, &header, source, cancel.clone(), progress).await?;
        }
        Ok(())
    }

    /// A stream of the accepted push `id` arrived for the file at `placed`.
    pub fn receive_pushed(
        self: &Arc<Self>,
        id: TransferId,
        receiving: Arc<Receiving>,
        placed: Placed,
        stream: FileReceiver,
    ) {
        let Some((cancel, progress)) = self
            .registry()
            .transfers
            .get(&id)
            .map(|t| (t.cancel.subscribe(), t.progress.clone()))
        else {
            return stream.cancel();
        };
        let shared = self.clone();
        self.spawn(async move {
            let received = async {
                let part = PartFile::create(placed.target.clone())
                    .await
                    .map_err(|e| StreamError::Failed(format!("cannot create the file: {e}")))?;
                receive_file(stream, part, placed.size, cancel, &progress).await
            };
            match received.await {
                Ok(_) if receiving.file_done() => shared.complete_push(id, &receiving),
                Ok(_) => {}
                Err(error) => shared.fail(id, error),
            }
        });
    }

    pub(super) fn complete_push(&self, id: TransferId, receiving: &Receiving) {
        let folder = Some(receiving.plan().destination().to_path_buf());
        if self.finish(id, Outcome::Completed { folder }) {
            self.send(TransferMessage::Completed { id });
        }
    }

    /// The other PC received our whole push `id`.
    pub fn on_completed(&self, id: TransferId) {
        let sending = matches!(
            self.registry().transfers.get(&id),
            Some(t) if matches!(t.stage, Stage::Sending) && t.info.kind == Kind::Push
        );
        if sending {
            self.finish(id, Outcome::Completed { folder: None });
        }
    }
}
