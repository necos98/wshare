//! What every task of a session's transfers shares: the file channel, the
//! way to the control stream, the observer, the permissions and the
//! registry. Also the one place transfers end, so each ends exactly once:
//! [`Shared::finish`] takes it out of the registry (which cancels whatever
//! of it still runs) and reports how it ended.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use net::FileChannel;
use protocol::control::ControlMessage;
use protocol::transfer::{TransferError, TransferId, TransferMessage};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::AbortHandle;

use super::events::{Outcome, TransferEvent, TransferInfo, TransferObserver};
use super::registry::{Registry, Stage, Transfer};
use super::streams::StreamError;

/// What the settings and permissions of the session allow, asked at the
/// moment it matters (they may change while the session lasts).
pub trait TransferPermissions: Send + Sync + 'static {
    /// This PC may push files to the other one.
    fn may_send(&self) -> bool;
    /// The other PC may push files to this one.
    fn accept_push(&self) -> bool;
    /// The other PC may fetch the files this PC's clipboard offers.
    fn serve_clipboard(&self) -> bool;
}

pub struct Shared {
    pub files: FileChannel,
    outbox: UnboundedSender<ControlMessage>,
    observer: Arc<dyn TransferObserver>,
    pub permissions: Arc<dyn TransferPermissions>,
    /// Where pushed files land.
    pub destination: PathBuf,
    registry: Mutex<Registry>,
    tasks: Mutex<Vec<AbortHandle>>,
}

impl Shared {
    pub fn new(
        files: FileChannel,
        outbox: UnboundedSender<ControlMessage>,
        observer: Arc<dyn TransferObserver>,
        permissions: Arc<dyn TransferPermissions>,
        destination: PathBuf,
        registry: Registry,
    ) -> Self {
        Self {
            files,
            outbox,
            observer,
            permissions,
            destination,
            registry: Mutex::new(registry),
            tasks: Mutex::new(Vec::new()),
        }
    }

    pub fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs `task` until it ends or the hub goes.
    pub fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        let handle = tokio::spawn(task).abort_handle();
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks.retain(|task| !task.is_finished());
        tasks.push(handle);
    }

    /// Stops every task (the hub is going).
    pub fn abort_all(&self) {
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
        for task in tasks {
            task.abort();
        }
    }

    pub fn send(&self, message: TransferMessage) {
        // Fails only once the session is over: nobody to tell then.
        let _ = self.outbox.send(ControlMessage::Transfer(message));
    }

    pub fn emit(&self, event: TransferEvent) {
        self.observer.on_event(event);
    }

    /// Registers `transfer` and reports it started.
    pub fn begin(&self, registry: &mut Registry, transfer: Transfer) {
        let info = transfer.info.clone();
        registry.transfers.insert(info.id, transfer);
        self.emit(TransferEvent::Started(info));
    }

    /// Reports a transfer that ended before it began (nothing to send, not
    /// allowed): started and finished at once.
    pub fn stillborn(&self, info: TransferInfo, outcome: Outcome) {
        let id = info.id;
        self.emit(TransferEvent::Started(info));
        self.emit(TransferEvent::Finished { id, outcome });
    }

    /// Ends `id` with `outcome`: whatever of it still runs is cancelled.
    /// `false` if it had ended already.
    pub fn finish(&self, id: TransferId, outcome: Outcome) -> bool {
        let Some(transfer) = self.take(id) else {
            return false;
        };
        // Raised before the transfer is dropped, for tasks that look.
        transfer.cancel.send_replace(true);
        self.emit(TransferEvent::Finished { id, outcome });
        true
    }

    /// The user cancelled `id` here: the other PC is told.
    pub fn cancel_here(&self, id: TransferId) {
        if self.finish(id, Outcome::Cancelled { by_peer: false }) {
            self.send(TransferMessage::Cancel { id });
        }
    }

    /// A task of `id` stopped with `error`. A cancellation that did not
    /// come through [`Self::finish`] (Explorer let go of a pasted file)
    /// is a cancellation here; a failure is reported to both sides.
    pub fn fail(&self, id: TransferId, error: StreamError) {
        match error {
            StreamError::Cancelled => self.cancel_here(id),
            StreamError::Failed(detail) => {
                if self.finish(id, Outcome::Failed(detail.clone())) {
                    self.send(TransferMessage::Failed {
                        id,
                        reason: TransferError::Failed(detail),
                    });
                }
            }
        }
    }

    /// The other PC refused or failed `id`.
    pub fn failed_by_peer(&self, id: TransferId, reason: TransferError) {
        let offered = matches!(
            self.registry().transfers.get(&id).map(|t| &t.stage),
            Some(Stage::Offered(_))
        );
        let outcome = if offered {
            Outcome::Refused(reason)
        } else {
            Outcome::Failed(describe(&reason))
        };
        self.finish(id, outcome);
    }

    /// Every transfer still running ends as interrupted (the session is
    /// over).
    pub fn interrupt_all(&self) {
        let ids: Vec<TransferId> = self.registry().transfers.keys().copied().collect();
        for id in ids {
            self.finish(id, Outcome::Interrupted);
        }
    }

    /// Takes `id` out of the registry, releasing the paths it reserved.
    fn take(&self, id: TransferId) -> Option<Transfer> {
        let mut registry = self.registry();
        let transfer = registry.transfers.remove(&id)?;
        if let Stage::Receiving(receiving) = &transfer.stage {
            registry.release(receiving.plan().roots());
        }
        Some(transfer)
    }
}

/// The English text of a reason, for the outcome of a transfer under way.
pub fn describe(reason: &TransferError) -> String {
    match reason {
        TransferError::NotAllowed => "not allowed by the other computer".into(),
        TransferError::Invalid => "refused as invalid".into(),
        TransferError::Unavailable => "the files are no longer available".into(),
        TransferError::Failed(detail) => detail.clone(),
    }
}
