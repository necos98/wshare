//! [`Desktop`] on the real Windows shell (see `crate::desktop`): the dialogs
//! run on threads of their own and answer through the interface's queue.

use std::path::Path;

use anyhow::Result;

use crate::desktop::{self, Choice};
use crate::text;

use super::ports::{AppEvent, ChoicePurpose, Desktop, Inbox};

pub struct WindowsShell {
    inbox: Inbox,
}

impl WindowsShell {
    pub fn new(inbox: Inbox) -> Self {
        Self { inbox }
    }
}

impl Desktop for WindowsShell {
    fn choose(&self, purpose: ChoicePurpose) {
        let (choice, title) = match purpose {
            ChoicePurpose::SendToViewer | ChoicePurpose::SendFromViewer(_) => {
                (Choice::Files, text::FILES_PICK_TITLE)
            }
            ChoicePurpose::Downloads => (Choice::Folder, text::DOWNLOADS_PICK_TITLE),
        };
        let inbox = self.inbox.clone();
        desktop::choose(choice, title, move |paths| {
            inbox.send(AppEvent::Chosen { purpose, paths });
        });
    }

    fn open_folder(&self, folder: &Path) -> Result<()> {
        desktop::open_folder(folder)
    }
}
