//! What the interface does with files: choosing those to send (to the
//! viewer connected here, or from a viewer window to its host), cancelling
//! transfers, opening the folder of received files, choosing that folder,
//! and the notification when the viewer's files arrived.

use std::path::{Path, PathBuf};

use protocol::transfer::TransferId;

use super::super::model::ViewerId;
use super::super::notifications;
use super::super::ports::{
    ChoicePurpose, Desktop, KnownHostsStore, SettingsBackend, SharingService, ViewerLauncher,
    WakeOnLan,
};
use super::Controller;
use crate::text;
use crate::transfer::{TransferCommand, TransferEvent};

impl<
    S: SharingService,
    L: ViewerLauncher,
    K: KnownHostsStore,
    B: SettingsBackend,
    W: WakeOnLan,
    D: Desktop,
> Controller<S, L, K, B, W, D>
{
    /// Opens the dialog for `purpose`; the choice comes back as
    /// `AppEvent::Chosen`.
    pub(super) fn choose(&mut self, purpose: ChoicePurpose) {
        self.state.files_notice = None;
        self.desktop.choose(purpose);
    }

    /// What the user chose in the dialog opened for `purpose`.
    pub(super) fn chosen(&mut self, purpose: ChoicePurpose, paths: Vec<PathBuf>) {
        let Some(first) = paths.first().cloned() else {
            return; // Cancelled.
        };
        match purpose {
            ChoicePurpose::SendToViewer => {
                let sent = self
                    .sharing
                    .as_ref()
                    .is_some_and(|sharing| sharing.send_files(paths));
                if !sent {
                    self.state.files_notice = Some(text::NO_VIEWER_TO_SEND_TO.into());
                }
            }
            ChoicePurpose::SendFromViewer(viewer) => {
                self.command_viewer(viewer, TransferCommand::Send(paths));
            }
            ChoicePurpose::Downloads => {
                let mut settings = self.state.settings.clone();
                settings.downloads = first.display().to_string();
                self.change_settings(settings);
            }
        }
    }

    /// Cancels a transfer of the viewer connected here (`None`: all).
    pub(super) fn cancel_host_transfer(&mut self, transfer: Option<TransferId>) {
        if let Some(sharing) = &self.sharing {
            sharing.cancel_transfer(transfer);
        }
    }

    /// Cancels a transfer of the viewer window `viewer` (`None`: all).
    pub(super) fn cancel_viewer_transfer(
        &mut self,
        viewer: ViewerId,
        transfer: Option<TransferId>,
    ) {
        let command = transfer.map_or(TransferCommand::CancelAll, TransferCommand::Cancel);
        self.command_viewer(viewer, command);
    }

    pub(super) fn open_folder(&mut self, folder: &Path) {
        self.state.files_notice = self
            .desktop
            .open_folder(folder)
            .err()
            .map(|e| text::folder_not_opened(&format!("{e:#}")));
    }

    /// A transfer of the viewer connected here moved on: the state follows,
    /// and its end may deserve a notification.
    pub(super) fn host_transfer(&mut self, event: TransferEvent) {
        let ended = self.state.sharing.files.apply(event);
        if let Some(ended) = ended
            && self.state.settings.notifications
        {
            let viewer = self.state.sharing.viewer.as_ref().map(|v| v.name.as_str());
            self.state
                .notifications
                .extend(notifications::for_transfer(&ended, viewer));
        }
    }

    fn command_viewer(&mut self, viewer: ViewerId, command: TransferCommand) {
        self.state.files_notice = self
            .launcher
            .command(viewer, command)
            .err()
            .map(|e| text::viewer_not_reached(&format!("{e:#}")));
    }
}
