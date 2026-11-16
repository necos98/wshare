//! The transfers of one connection: what the control loop of the host or
//! of the viewer talks to. It takes the transfer messages the other PC
//! sends and the user's commands, and gives back the messages to send
//! (on the receiver [`TransferHub::start`] returns, for the control loop
//! to forward). Each transfer runs in tasks of its own; dropping the hub
//! stops them all, deletes the partial files and reports every transfer
//! still running as interrupted.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use net::{FileChannel, FileReceiver, IncomingFile};
use protocol::control::ControlMessage;
#[cfg(test)]
use protocol::transfer::OfferId;
use protocol::transfer::{StreamHeader, TransferError, TransferMessage};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::events::{TransferCommand, TransferEvent, TransferObserver};
#[cfg(test)]
use super::listing::Listing;
use super::listing::{self, ListingError};
use super::paste::PasteLink;
use super::registry::{Registry, Role, Stage};
use super::shared::{Shared, TransferPermissions};
use super::streams::StreamError;

/// How often progress is reported while something moves.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// How a session's transfers are set up.
pub struct HubConfig {
    pub role: Role,
    /// Where pushed files land.
    pub destination: PathBuf,
}

pub struct TransferHub {
    shared: Arc<Shared>,
    pastes: PasteLink,
}

impl TransferHub {
    /// Starts taking file streams on `files`. Must run inside the tokio
    /// runtime. The returned receiver carries the messages for the other PC.
    pub fn start(
        files: FileChannel,
        config: HubConfig,
        permissions: Arc<dyn TransferPermissions>,
        observer: Arc<dyn TransferObserver>,
    ) -> (Self, UnboundedReceiver<ControlMessage>) {
        let (outbox, messages) = unbounded_channel();
        let shared = Arc::new(Shared::new(
            files,
            outbox,
            observer,
            permissions,
            config.destination,
            Registry::new(config.role),
        ));
        let (pastes, requests) = PasteLink::new();
        shared.spawn(accept_streams(shared.clone()));
        shared.spawn(report_progress(shared.clone()));
        shared.spawn(shared.clone().take_pastes(requests));
        (Self { shared, pastes }, messages)
    }

    /// A transfer message from the other PC. `ClipboardFiles` is not for
    /// the hub (the clipboard takes it) and is ignored here.
    pub fn on_message(&self, message: TransferMessage) {
        let shared = &self.shared;
        match message {
            TransferMessage::Offer { id, entries } => shared.on_offer(id, entries),
            TransferMessage::Accept { id } => shared.on_accept(id),
            TransferMessage::Failed { id, reason } => shared.failed_by_peer(id, reason),
            TransferMessage::Request { offer, index, id } => shared.on_request(offer, index, id),
            TransferMessage::Cancel { id } => {
                shared.finish(id, super::events::Outcome::Cancelled { by_peer: true });
            }
            TransferMessage::Completed { id } => shared.on_completed(id),
            TransferMessage::ClipboardFiles { .. } => {}
        }
    }

    pub fn command(&self, command: TransferCommand) {
        match command {
            TransferCommand::Send(picked) => self.shared.start_push(picked),
            TransferCommand::Cancel(id) => self.shared.cancel_here(id),
            TransferCommand::CancelAll => {
                let ids: Vec<_> = self.shared.registry().transfers.keys().copied().collect();
                for id in ids {
                    self.shared.cancel_here(id);
                }
            }
        }
    }

    /// Files were copied on this PC: walks them (off the control loop) and
    /// announces them to the other PC, which can then paste them. Any
    /// earlier offer ends now; this one is dropped if the clipboard changes
    /// again before the walk is over. `on_error` hears why they cannot be
    /// offered (too many, unreadable...).
    pub fn offer_clipboard_files(
        &self,
        picked: Vec<PathBuf>,
        on_error: impl FnOnce(ListingError) + Send + 'static,
    ) {
        let generation = self.shared.registry().withdraw_clipboard();
        let shared = self.shared.clone();
        self.shared.spawn(async move {
            match tokio::task::spawn_blocking(move || listing::list(&picked)).await {
                Ok(Ok(listing)) => {
                    let entries = listing.entries.clone();
                    let offer = shared.registry().offer_clipboard_if(generation, listing);
                    if let Some(offer) = offer {
                        shared.send(TransferMessage::ClipboardFiles { offer, entries });
                    }
                }
                Ok(Err(error)) => on_error(error),
                // The walk was stopped: the session is ending.
                Err(_) => {}
            }
        });
    }

    /// Offers `listing` as this PC's clipboard files, replacing any earlier
    /// offer; returns the id to announce them with.
    #[cfg(test)]
    pub fn offer_clipboard(&self, listing: Listing) -> OfferId {
        self.shared.registry().offer_clipboard(listing)
    }

    /// The clipboard no longer holds the files offered: they cannot be
    /// pasted any more.
    pub fn withdraw_clipboard(&self) {
        self.shared.registry().withdraw_clipboard();
    }

    /// The permissions may have been taken back: the pushes arriving and
    /// the pastes from the other PC no longer allowed stop now.
    pub fn enforce_permissions(&self) {
        self.shared.enforce_permissions();
    }

    /// For the clipboard files the other PC offers: how their bytes are
    /// fetched when Explorer pastes them.
    pub fn paste_link(&self) -> PasteLink {
        self.pastes.clone()
    }
}

impl Drop for TransferHub {
    fn drop(&mut self) {
        self.shared.abort_all();
        self.shared.interrupt_all();
    }
}

/// Takes every file stream the other PC opens, reading its header on a
/// task of its own.
async fn accept_streams(shared: Arc<Shared>) {
    while let Some(incoming) = shared.files.accept().await {
        let shared_for_stream = shared.clone();
        shared.spawn(open_stream(shared_for_stream, incoming));
    }
}

async fn open_stream(shared: Arc<Shared>, incoming: IncomingFile) {
    // A stream without a valid header was stopped: nothing to route.
    if let Ok((header, stream)) = incoming.open().await {
        route(&shared, &header, stream);
    }
}

/// Which transfer a stream belongs to.
enum Target {
    Push(Arc<super::receiving::Receiving>),
    Paste,
    /// A transfer no longer allowed here.
    Revoked,
    Unexpected,
}

/// Hands a stream to the transfer its header names, if it expects one and
/// is still allowed.
fn route(shared: &Arc<Shared>, header: &StreamHeader, stream: FileReceiver) {
    // Looked up in a statement of its own: the registry's lock ends with
    // it, before the handlers below take it again.
    let target = match shared
        .registry()
        .transfers
        .get(&header.id)
        .map(|t| &t.stage)
    {
        Some(stage) if !shared.admits_stream(stage) => Target::Revoked,
        Some(Stage::Receiving(receiving)) => Target::Push(receiving.clone()),
        Some(Stage::Pulling { .. }) => Target::Paste,
        _ => Target::Unexpected,
    };
    let receiving = match target {
        Target::Push(receiving) => receiving,
        Target::Paste => return shared.forward_pasted(header, stream),
        Target::Revoked => {
            stream.cancel();
            return shared.revoke(header.id);
        }
        Target::Unexpected => return stream.cancel(),
    };
    match receiving.claim(header.index, &header.entry) {
        Some(placed) => shared.receive_pushed(header.id, receiving, placed, stream),
        None => {
            stream.cancel();
            shared.fail(header.id, StreamError::Failed(TransferError::NotInList));
        }
    }
}

async fn report_progress(shared: Arc<Shared>) {
    let mut ticks = tokio::time::interval(PROGRESS_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        let moved = shared.registry().moved();
        for (id, done_bytes) in moved {
            shared.emit(TransferEvent::Progress { id, done_bytes });
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod failure_tests;
#[cfg(test)]
mod guard_tests;
#[cfg(test)]
mod test_support;
