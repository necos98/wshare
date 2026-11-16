//! The file transfers of the host's session: the hub of the connection,
//! with the permissions read live from the service's state, its events
//! reported as [`HostEvent::Transfer`], and the commands of the front-end
//! (see [`super::HostHandle::send_files`]) taken while the session lasts.

use std::sync::Arc;

use net::FileChannel;
use protocol::control::ControlMessage;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::sync::watch;

use super::HostEvent;
use super::events::EventSink;
use super::session::SessionContext;
use super::state::SharedState;
use crate::clipboard::HostRules;
use crate::transfer::{
    HubConfig, Role, TransferCommand, TransferEvent, TransferHub, TransferObserver,
    TransferPermissions,
};

/// The settings, as they are at the moment a transfer asks.
struct HostPermissions {
    input: watch::Receiver<bool>,
    clipboard: watch::Receiver<bool>,
    receive: watch::Receiver<bool>,
}

impl HostPermissions {
    fn rules(&self) -> HostRules {
        HostRules {
            sharing: *self.clipboard.borrow(),
            input_allowed: *self.input.borrow(),
            receive_files: *self.receive.borrow(),
        }
    }
}

impl TransferPermissions for HostPermissions {
    /// The person at the host decides what to send.
    fn may_send(&self) -> bool {
        true
    }

    fn accept_push(&self) -> bool {
        self.rules().accept_pushed_files()
    }

    fn serve_clipboard(&self) -> bool {
        self.rules().send_files_to_viewer()
    }
}

struct Forward(EventSink);

impl TransferObserver for Forward {
    fn on_event(&self, event: TransferEvent) {
        self.0.emit(HostEvent::Transfer(event));
    }
}

/// The transfers of one session, from Welcome to its end.
pub(super) struct SessionTransfers {
    pub(super) hub: TransferHub,
    outgoing: UnboundedReceiver<ControlMessage>,
    commands: UnboundedReceiver<TransferCommand>,
    state: Arc<SharedState>,
}

impl SessionTransfers {
    pub(super) fn start(files: FileChannel, ctx: &SessionContext) -> Self {
        let permissions = HostPermissions {
            input: ctx.state.watch_input(),
            clipboard: ctx.state.watch_clipboard(),
            receive: ctx.state.watch_receive_files(),
        };
        let config = HubConfig {
            role: Role::Host,
            destination: ctx.downloads.clone(),
        };
        let observer = Forward(ctx.events.clone());
        let (hub, outgoing) =
            TransferHub::start(files, config, Arc::new(permissions), Arc::new(observer));
        let (commands_tx, commands) = unbounded_channel();
        ctx.state.set_transfers(Some(commands_tx));
        Self {
            hub,
            outgoing,
            commands,
            state: Arc::clone(&ctx.state),
        }
    }

    /// The next message for the viewer, applying the front-end's commands
    /// meanwhile. Cancel-safe.
    pub(super) async fn next(&mut self) -> ControlMessage {
        loop {
            tokio::select! {
                Some(message) = self.outgoing.recv() => return message,
                Some(command) = self.commands.recv() => self.hub.command(command),
                else => std::future::pending::<()>().await,
            }
        }
    }
}

impl Drop for SessionTransfers {
    fn drop(&mut self) {
        // The next viewer's transfers are its own.
        self.state.set_transfers(None);
    }
}
