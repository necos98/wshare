//! Permissions taken back while transfers run: the files the other PC
//! pushes, or this PC pastes from it, stop as soon as they are no longer
//! allowed here, whether the change is noticed by the session
//! ([`Shared::enforce_permissions`]) or by a stream arriving
//! ([`Shared::admits_stream`]).

use protocol::transfer::{TransferError, TransferId, TransferMessage};

use super::events::{Failure, Outcome};
use super::registry::Stage;
use super::shared::Shared;

impl Shared {
    /// The settings changed: the pushes arriving and the pastes from the
    /// other PC that are no longer allowed stop, and the other PC is told.
    pub fn enforce_permissions(&self) {
        let pushes_allowed = self.permissions.accept_push();
        let pastes_allowed = self.permissions.paste_from_peer();
        if pushes_allowed && pastes_allowed {
            return;
        }
        let revoked: Vec<TransferId> = self
            .registry()
            .transfers
            .iter()
            .filter(|(_, transfer)| match transfer.stage {
                Stage::Planning { .. } | Stage::Receiving(_) => !pushes_allowed,
                Stage::Pulling { .. } => !pastes_allowed,
                Stage::Offered(_) | Stage::Sending => false,
            })
            .map(|(id, _)| *id)
            .collect();
        for id in revoked {
            self.revoke(id);
        }
    }

    /// Whether a stream may still feed `stage`: a push arriving while
    /// pushes are allowed, a pasted file while pastes are. A stream for
    /// anything else is not this check's business.
    pub fn admits_stream(&self, stage: &Stage) -> bool {
        match stage {
            Stage::Receiving(_) => self.permissions.accept_push(),
            Stage::Pulling { .. } => self.permissions.paste_from_peer(),
            Stage::Planning { .. } | Stage::Offered(_) | Stage::Sending => true,
        }
    }

    /// Stops `id`, which is no longer allowed, and tells the other PC.
    pub fn revoke(&self, id: TransferId) {
        if self.finish(id, Outcome::Failed(Failure::NoLongerAllowed)) {
            let reason = TransferError::NotAllowed;
            self.send(TransferMessage::Failed { id, reason });
        }
    }
}
