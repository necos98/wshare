//! The viewer's file transfers: what it may do (follow the host's remote
//! control permission, like the clipboard), where their events go (the
//! window, the terminal and, when launched by it, the desktop interface),
//! and a hub for each connection (a reconnection starts a new one).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use net::FileChannel;
use protocol::control::ControlMessage;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use winit::event_loop::EventLoopProxy;

use super::channel;
use super::window::UserEvent;
use crate::clipboard::ViewerRules;
use crate::paths;
use crate::transfer::{
    HubConfig, Role, TransferCommand, TransferEvent, TransferHub, TransferObserver,
    TransferPermissions, terminal,
};

/// What this viewer may do with files, kept up to date with the host's
/// permission for remote control.
pub struct ViewerPermissions {
    host_allows: AtomicBool,
    view_only: bool,
}

impl ViewerPermissions {
    pub fn new(rules: ViewerRules) -> Self {
        Self {
            host_allows: AtomicBool::new(rules.host_allows),
            view_only: rules.view_only,
        }
    }

    pub fn set_host_allows(&self, allowed: bool) {
        self.host_allows.store(allowed, Ordering::Relaxed);
    }

    fn rules(&self) -> ViewerRules {
        ViewerRules {
            host_allows: self.host_allows.load(Ordering::Relaxed),
            view_only: self.view_only,
        }
    }
}

impl TransferPermissions for ViewerPermissions {
    fn may_send(&self) -> bool {
        self.rules().send_files_to_host()
    }

    /// What the host sends lands in this PC's download folder: the person
    /// at the host chose to send it to this viewer.
    fn accept_push(&self) -> bool {
        true
    }

    fn serve_clipboard(&self) -> bool {
        self.rules().send_files_to_host()
    }
}

/// Tells the window, the terminal and (when it asked) the interface.
pub struct ViewerObserver {
    window: Mutex<EventLoopProxy<UserEvent>>,
    reporting: bool,
}

impl ViewerObserver {
    pub fn new(window: EventLoopProxy<UserEvent>, reporting: bool) -> Self {
        Self {
            window: Mutex::new(window),
            reporting,
        }
    }
}

impl TransferObserver for ViewerObserver {
    fn on_event(&self, event: TransferEvent) {
        if let Some(line) = terminal::describe(&event) {
            println!("{line}");
        }
        if self.reporting {
            println!("{}", channel::event_line(&event));
        }
        let window = self.window.lock().unwrap_or_else(|e| e.into_inner());
        // Fails only once the window is gone.
        let _ = window.send_event(UserEvent::Transfer(event));
    }
}

/// What every connection's transfers are started with.
pub struct TransferSetup {
    pub destination: PathBuf,
    pub permissions: Arc<ViewerPermissions>,
    pub observer: Arc<ViewerObserver>,
}

impl TransferSetup {
    /// The transfers of this viewer: files land in `downloads` (`None`: this
    /// user's `Downloads\wshare`). Returns also the way to send them file
    /// commands, and its receiving end; when the interface launched the
    /// viewer (`reporting`) its commands on stdin are read too.
    pub fn new(
        downloads: Option<PathBuf>,
        rules: ViewerRules,
        window: EventLoopProxy<UserEvent>,
        reporting: bool,
    ) -> (
        Self,
        UnboundedSender<TransferCommand>,
        UnboundedReceiver<TransferCommand>,
    ) {
        let (commands, received) = unbounded_channel();
        // Only the interface writes commands; a terminal's stdin stays alone.
        if reporting && let Err(e) = channel::read_commands(commands.clone()) {
            eprintln!("warning: cannot read the interface's commands: {e}");
        }
        let setup = Self {
            destination: downloads.unwrap_or_else(paths::default_downloads),
            permissions: Arc::new(ViewerPermissions::new(rules)),
            observer: Arc::new(ViewerObserver::new(window, reporting)),
        };
        (setup, commands, received)
    }

    /// The transfers of a new connection on `files`; the receiver carries
    /// the messages for the host.
    pub fn start(&self, files: FileChannel) -> (TransferHub, UnboundedReceiver<ControlMessage>) {
        let config = HubConfig {
            role: Role::Viewer,
            destination: self.destination.clone(),
        };
        TransferHub::start(
            files,
            config,
            self.permissions.clone(),
            self.observer.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_viewer_sends_files_only_while_it_may_control_the_host() {
        let permissions = ViewerPermissions::new(ViewerRules {
            host_allows: false,
            view_only: false,
        });
        assert!(!permissions.may_send());
        assert!(!permissions.serve_clipboard());
        assert!(permissions.accept_push(), "the host's files always land");
        permissions.set_host_allows(true);
        assert!(permissions.may_send());
        assert!(permissions.serve_clipboard());

        let view_only = ViewerPermissions::new(ViewerRules {
            host_allows: true,
            view_only: true,
        });
        assert!(!view_only.may_send());
        assert!(view_only.accept_push());
    }
}
