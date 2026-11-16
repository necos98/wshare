//! Copy and paste of files: serving the files this PC's clipboard offers
//! when the other PC pastes them, and fetching a file the other PC offers
//! when Explorer here reads it (one transfer per file).

use std::sync::Arc;
use std::time::Duration;

use net::FileReceiver;
use protocol::transfer::{
    FileEntry, OfferId, StreamHeader, TransferError, TransferId, TransferMessage,
};
use tokio::sync::mpsc;

use super::events::{Direction, Kind, Outcome, TransferInfo};
use super::registry::{Chunk, Stage, Transfer};
use super::shared::Shared;
use super::streams::{StreamError, cancelled, forward_file, send_file};

/// Chunks of a pasted file waiting for Explorer: a few hundred KiB each.
const PASTE_QUEUE: usize = 8;

/// How long the other PC has to start sending a file asked for.
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(30);

/// Explorer reads entry `index` of the clipboard files `offer`.
pub struct PasteRequest {
    pub offer: OfferId,
    pub index: u32,
    pub entry: FileEntry,
    pub chunks: mpsc::Sender<Chunk>,
}

/// How a pasted file reaches the session that fetches it. Cloneable and
/// thread-safe: the clipboard's thread holds it.
#[derive(Clone)]
pub struct PasteLink(mpsc::UnboundedSender<PasteRequest>);

impl PasteLink {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<PasteRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self(tx), rx)
    }

    /// Asks for entry `index` of `offer`; its bytes come on the receiver,
    /// which closes without [`Chunk::End`] if the transfer fails. `None`
    /// once the session is over.
    pub fn fetch(
        &self,
        offer: OfferId,
        index: u32,
        entry: FileEntry,
    ) -> Option<mpsc::Receiver<Chunk>> {
        let (chunks, rx) = mpsc::channel(PASTE_QUEUE);
        let request = PasteRequest {
            offer,
            index,
            entry,
            chunks,
        };
        self.0.send(request).ok().map(|()| rx)
    }
}

fn paste_info(id: TransferId, direction: Direction, entry: &FileEntry) -> TransferInfo {
    TransferInfo {
        id,
        direction,
        kind: Kind::Paste,
        name: entry
            .path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string(),
        files: 1,
        total_bytes: entry.size,
    }
}

impl Shared {
    /// Takes Explorer's requests to the other PC, one transfer each.
    pub async fn take_pastes(self: Arc<Self>, mut requests: mpsc::UnboundedReceiver<PasteRequest>) {
        while let Some(request) = requests.recv().await {
            self.ask_for(request);
        }
    }

    fn ask_for(self: &Arc<Self>, request: PasteRequest) {
        let PasteRequest {
            offer,
            index,
            entry,
            chunks,
        } = request;
        // Dropping `chunks` fails Explorer's read at once: when the other
        // PC's files may no longer be pasted here (what the clipboard
        // still holds of them stays unreadable), or too much runs.
        if !self.permissions.paste_from_peer() {
            return;
        }
        let mut registry = self.registry();
        if registry.is_full() {
            return;
        }
        let id = registry.allocate();
        let waiting = chunks.clone();
        let info = paste_info(id, Direction::Receiving, &entry);
        let transfer = Transfer::new(
            info,
            Stage::Pulling {
                entry,
                chunks: Some(chunks),
            },
        );
        let mut cancel = transfer.cancel.subscribe();
        self.begin(&mut registry, transfer);
        drop(registry);
        self.send(TransferMessage::Request { offer, index, id });
        // Gives up when nothing comes in time, or Explorer lets go first.
        let shared = self.clone();
        self.spawn(async move {
            tokio::select! {
                () = cancelled(&mut cancel) => {}
                () = waiting.closed() => shared.cancel_here(id),
                () = tokio::time::sleep(FIRST_BYTE_TIMEOUT) => {
                    if shared.still_waiting(id) {
                        shared.fail(id, StreamError::Failed(TransferError::NotSent));
                    }
                }
            }
        });
    }

    fn still_waiting(&self, id: TransferId) -> bool {
        matches!(
            self.registry().transfers.get(&id).map(|t| &t.stage),
            Some(Stage::Pulling {
                chunks: Some(_),
                ..
            })
        )
    }

    /// The stream of the file asked for as `id` arrived: on to Explorer.
    pub fn forward_pasted(self: &Arc<Self>, header: &StreamHeader, stream: FileReceiver) {
        let id = header.id;
        let mut registry = self.registry();
        let Some(transfer) = registry.transfers.get_mut(&id) else {
            return stream.cancel();
        };
        let Stage::Pulling { entry, chunks } = &mut transfer.stage else {
            return stream.cancel();
        };
        if *entry != header.entry {
            drop(registry);
            stream.cancel();
            return self.fail(id, StreamError::Failed(TransferError::NotAsked));
        }
        let Some(chunks) = chunks.take() else {
            return stream.cancel();
        };
        let size = entry.size;
        let cancel = transfer.cancel.subscribe();
        let progress = transfer.progress.clone();
        drop(registry);
        let shared = self.clone();
        self.spawn(async move {
            match forward_file(stream, chunks, size, cancel, &progress).await {
                Ok(()) => {
                    shared.finish(id, Outcome::Completed { folder: None });
                }
                Err(error) => shared.fail(id, error),
            }
        });
    }

    /// The other PC pastes entry `index` of our clipboard files `offer`.
    pub fn on_request(self: &Arc<Self>, offer: OfferId, index: u32, id: TransferId) {
        let mut registry = self.registry();
        if !registry.admits_peer(id) {
            drop(registry);
            let reason = TransferError::TooManyTransfers;
            return self.send(TransferMessage::Failed { id, reason });
        }
        if !self.permissions.serve_clipboard() {
            drop(registry);
            let reason = TransferError::NotAllowed;
            return self.send(TransferMessage::Failed { id, reason });
        }
        let Some((source, entry)) = registry.clipboard_file(offer, index) else {
            drop(registry);
            let reason = TransferError::Unavailable;
            return self.send(TransferMessage::Failed { id, reason });
        };
        let transfer = Transfer::new(paste_info(id, Direction::Sending, &entry), Stage::Sending);
        let cancel = transfer.cancel.subscribe();
        let progress = transfer.progress.clone();
        self.begin(&mut registry, transfer);
        drop(registry);
        let header = StreamHeader { id, index, entry };
        let shared = self.clone();
        self.spawn(async move {
            match send_file(&shared.files, &header, &source, cancel, &progress).await {
                Ok(()) => {
                    shared.finish(id, Outcome::Completed { folder: None });
                }
                Err(error) => shared.fail(id, error),
            }
        });
    }
}
