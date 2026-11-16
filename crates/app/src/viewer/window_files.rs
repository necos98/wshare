//! The viewer window's side of file transfers: files dropped on it or
//! chosen from the "File" menu go to the host, the transfers' events turn
//! into the progress line, the end messages and what the menu offers.
//! Pure apart from the commands it sends: the window applies what it says.

use std::path::PathBuf;

use tokio::sync::mpsc::UnboundedSender;

use super::bar_menu::FileState;
use crate::text;
use crate::transfer::{Direction, Outcome, TransferBoard, TransferCommand, TransferEvent};

/// What the window must show after a transfer event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesView {
    /// The line of the transfers in progress (`None`: none left).
    pub progress: Option<String>,
    /// A transfer ended: what to tell the user.
    pub message: Option<String>,
    /// What the "File" menu offers now.
    pub menu: FileState,
}

pub struct WindowFiles {
    commands: UnboundedSender<TransferCommand>,
    board: TransferBoard,
    /// Where the host's files landed last.
    received: Option<PathBuf>,
    /// Dropped since the window last looked: one drop of several files
    /// arrives as one event per file, sent together.
    dropped: Vec<PathBuf>,
}

impl WindowFiles {
    pub fn new(commands: UnboundedSender<TransferCommand>) -> Self {
        Self {
            commands,
            board: TransferBoard::default(),
            received: None,
            dropped: Vec::new(),
        }
    }

    pub fn dropped(&mut self, path: PathBuf) {
        self.dropped.push(path);
    }

    /// Sends what was dropped, as one transfer.
    pub fn send_dropped(&mut self) {
        if !self.dropped.is_empty() {
            let dropped = std::mem::take(&mut self.dropped);
            self.command(TransferCommand::Send(dropped));
        }
    }

    /// Sends the files chosen in the dialog, if any.
    pub fn send_chosen(&mut self, chosen: Vec<PathBuf>) {
        if !chosen.is_empty() {
            self.command(TransferCommand::Send(chosen));
        }
    }

    pub fn cancel_all(&self) {
        self.command(TransferCommand::CancelAll);
    }

    /// Where the host's files landed last, if any arrived.
    pub fn received_folder(&self) -> Option<&PathBuf> {
        self.received.as_ref()
    }

    pub fn on_event(&mut self, event: TransferEvent) -> FilesView {
        let message = self.board.apply(event).map(|ended| {
            if let (
                Direction::Receiving,
                Outcome::Completed {
                    folder: Some(folder),
                },
            ) = (ended.info.direction, &ended.outcome)
            {
                self.received = Some(folder.clone());
            }
            text::transfer_ended(&ended)
        });
        FilesView {
            progress: self.board.summary().map(|s| text::transfer_summary(&s)),
            message,
            menu: self.menu(),
        }
    }

    pub fn menu(&self) -> FileState {
        FileState {
            transfers: !self.board.is_empty(),
            received: self.received.is_some(),
        }
    }

    fn command(&self, command: TransferCommand) {
        // Fails only once the session is over: nothing to send to then.
        let _ = self.commands.send(command);
    }
}

#[cfg(test)]
mod tests {
    use protocol::transfer::TransferId;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    use super::*;
    use crate::transfer::{Kind, TransferInfo};

    fn files() -> (WindowFiles, UnboundedReceiver<TransferCommand>) {
        let (tx, rx) = unbounded_channel();
        (WindowFiles::new(tx), rx)
    }

    fn started(id: u32, direction: Direction) -> TransferEvent {
        TransferEvent::Started(TransferInfo {
            id: TransferId(id),
            direction,
            kind: Kind::Push,
            name: "Foto".into(),
            files: 2,
            total_bytes: 1000,
        })
    }

    #[test]
    fn several_dropped_files_leave_as_one_transfer() {
        let (mut files, mut commands) = files();
        files.send_dropped();
        assert!(commands.try_recv().is_err(), "nothing dropped");
        files.dropped(PathBuf::from("C:\\a"));
        files.dropped(PathBuf::from("C:\\b"));
        files.send_dropped();
        assert_eq!(
            commands.try_recv(),
            Ok(TransferCommand::Send(vec![
                PathBuf::from("C:\\a"),
                PathBuf::from("C:\\b")
            ]))
        );
        files.send_dropped();
        assert!(commands.try_recv().is_err(), "sent once");
    }

    #[test]
    fn a_cancelled_dialog_sends_nothing() {
        let (mut files, mut commands) = files();
        files.send_chosen(Vec::new());
        assert!(commands.try_recv().is_err());
        files.cancel_all();
        assert_eq!(commands.try_recv(), Ok(TransferCommand::CancelAll));
    }

    #[test]
    fn progress_shows_while_transfers_run_and_the_end_is_told() {
        let (mut files, _commands) = files();
        let view = files.on_event(started(2, Direction::Receiving));
        assert!(
            view.progress
                .unwrap()
                .starts_with("Ricezione di «Foto»: 0%")
        );
        assert!(view.menu.transfers && !view.menu.received);

        let folder = PathBuf::from("C:\\Users\\a\\Downloads\\wshare");
        let view = files.on_event(TransferEvent::Finished {
            id: TransferId(2),
            outcome: Outcome::Completed {
                folder: Some(folder.clone()),
            },
        });
        assert_eq!(view.progress, None);
        assert_eq!(view.message.as_deref(), Some("Ricevuto «Foto»"));
        assert!(!view.menu.transfers && view.menu.received);
        assert_eq!(files.received_folder(), Some(&folder));
    }

    #[test]
    fn files_sent_do_not_name_a_folder_to_open() {
        let (mut files, _commands) = files();
        files.on_event(started(1, Direction::Sending));
        let view = files.on_event(TransferEvent::Finished {
            id: TransferId(1),
            outcome: Outcome::Completed { folder: None },
        });
        assert!(!view.menu.received);
    }
}
