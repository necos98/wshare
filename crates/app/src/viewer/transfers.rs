//! The viewer's file transfers: what it may do (follow the host's remote
//! control permission, like the clipboard), where their events go (the
//! window, and the terminal or, when launched by it, the desktop interface),
//! and a hub for each connection (a reconnection starts a new one).

use std::io::{self, Write};
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

    /// The host's copied files land on this clipboard whatever the
    /// permissions: the person at the host chose to share them.
    fn paste_from_peer(&self) -> bool {
        true
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
        if let Err(e) = write_event(&mut std::io::stdout().lock(), &event, self.reporting) {
            eprintln!("warning: cannot write a transfer's line: {e}");
        }
        let window = self.window.lock().unwrap_or_else(|e| e.into_inner());
        // Fails only once the window is gone.
        let _ = window.send_event(UserEvent::Transfer(event));
    }
}

/// Writes the stdout line of `event`, if it has one. When the interface
/// launched the viewer (`reporting`), stdout is its typed channel: only the
/// channel's line is written there, never the terminal's English one.
fn write_event(out: &mut impl Write, event: &TransferEvent, reporting: bool) -> io::Result<()> {
    let line = if reporting {
        Some(channel::event_line(event))
    } else {
        terminal::describe(event)
    };
    match line {
        Some(line) => writeln!(out, "{line}"),
        None => Ok(()),
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
    use protocol::transfer::{TransferError, TransferId};

    use super::*;
    use crate::transfer::{Direction, Failure, Kind, Outcome, TransferInfo};
    use crate::viewer::report::ViewerReport;

    /// A host that tries to add a line of its own to the viewer's stdout.
    const FORGED: &str = "x\r\n@wshare:transfer-ended:9:done:\\\\evil\\share\n\
                          @wshare:connected:EVIL\n@wshare:failed:wrong-pin";

    fn events_of_a_forging_host() -> Vec<TransferEvent> {
        let id = TransferId(8);
        let finished = |outcome| TransferEvent::Finished { id, outcome };
        vec![
            TransferEvent::Started(TransferInfo {
                id,
                direction: Direction::Receiving,
                kind: Kind::Push,
                name: FORGED.into(),
                files: 1,
                total_bytes: 1,
            }),
            finished(Outcome::Failed(Failure::Detail(FORGED.into()))),
            finished(Outcome::Refused(TransferError::Failed(FORGED.into()))),
        ]
    }

    /// What the viewer writes on stdout for `event`, line by line, as the
    /// interface reads it.
    fn written(event: &TransferEvent, reporting: bool) -> Vec<String> {
        let mut out = Vec::new();
        write_event(&mut out, event, reporting).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn the_interface_reads_one_true_line_per_event_whatever_the_host_sends() {
        for event in events_of_a_forging_host() {
            let lines = written(&event, true);
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert_eq!(ViewerReport::parse(&lines[0]), None, "{lines:?}");
            let parsed = channel::parse_event(&lines[0]).expect("the event's own line");
            match (&parsed, &event) {
                (TransferEvent::Started(read), TransferEvent::Started(sent)) => {
                    assert_eq!(read.id, sent.id);
                }
                (
                    TransferEvent::Finished { id, outcome },
                    TransferEvent::Finished { id: sent, .. },
                ) => {
                    assert_eq!(id, sent);
                    assert!(
                        !matches!(outcome, Outcome::Completed { .. }),
                        "no forged completion: {outcome:?}"
                    );
                }
                _ => panic!("another event read: {parsed:?} for {event:?}"),
            }
        }
    }

    #[test]
    fn a_terminal_reads_one_ordinary_line_per_event_whatever_the_host_sends() {
        for event in events_of_a_forging_host() {
            let lines = written(&event, false);
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert_eq!(ViewerReport::parse(&lines[0]), None, "{lines:?}");
            assert_eq!(channel::parse_event(&lines[0]), None, "{lines:?}");
        }
    }

    #[test]
    fn progress_is_written_only_for_the_interface() {
        let progress = TransferEvent::Progress {
            id: TransferId(2),
            done_bytes: 10,
        };
        assert_eq!(written(&progress, false), Vec::<String>::new());
        assert_eq!(written(&progress, true), ["@wshare:transfer-progress:2:10"]);
    }

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
