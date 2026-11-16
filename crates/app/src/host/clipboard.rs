//! The host's side of the shared clipboard during one session: the text and
//! files the host copies go to the viewer, and the viewer's go to the
//! host's clipboard, within the rules of [`HostRules`].
//!
//! A clipboard that cannot be used never ends the session: the problem is
//! reported as a warning and the session goes on without it.

use std::time::{Duration, Instant};

use protocol::control::ControlMessage;
use protocol::transfer::{FileEntry, OfferId};
use tokio::sync::watch;

use super::events::{EventSink, Throttle};
use super::{HostEvent, HostWarning};
use crate::clipboard::{
    AccessError, ClipboardAccess, ClipboardSync, FileSharing, HostRules, NotShared, Outgoing,
    Received, SystemClipboard, share_copied_files,
};

/// At most one warning about a failing clipboard this often: each change
/// the user makes or the viewer sends may fail the same way.
const FAILURE_WARNING_INTERVAL: Duration = Duration::from_secs(10);

/// The settings the clipboard follows, read when they matter.
pub(super) struct ClipboardSettings {
    /// "Condividi appunti".
    pub(super) sharing: watch::Receiver<bool>,
    /// "Consenti ricezione file".
    pub(super) receive_files: watch::Receiver<bool>,
}

/// What [`SessionClipboard::changed`] noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClipboardChange {
    /// Something was copied on the host.
    Copied,
    /// "Condividi appunti" or "Consenti ricezione file" changed.
    Settings,
}

pub(super) struct SessionClipboard<A = SystemClipboard> {
    /// `None` if the clipboard could not be listened to.
    link: Option<ClipboardSync<A>>,
    settings: ClipboardSettings,
    events: EventSink,
    failures: Throttle,
    /// The viewer's files were put on the host's clipboard and not taken
    /// off it since.
    holds_viewer_files: bool,
}

impl SessionClipboard {
    /// Starts listening to the clipboard for this session.
    pub(super) fn open(settings: ClipboardSettings, events: EventSink) -> Self {
        let link = ClipboardSync::open();
        if let Err(e) = &link {
            events.emit(HostEvent::Warning(HostWarning::ClipboardUnavailable(
                format!("{e:#}"),
            )));
        }
        Self::with_link(link.ok(), settings, events)
    }
}

impl<A: ClipboardAccess> SessionClipboard<A> {
    pub(super) fn with_link(
        link: Option<ClipboardSync<A>>,
        settings: ClipboardSettings,
        events: EventSink,
    ) -> Self {
        Self {
            link,
            settings,
            events,
            failures: Throttle::new(FAILURE_WARNING_INTERVAL),
            holds_viewer_files: false,
        }
    }

    /// The settings or remote control changed: the viewer's files are
    /// taken off the host's clipboard once they may no longer be pasted
    /// here (only if the clipboard still holds them).
    pub(super) fn withdraw_disallowed_files(&mut self, input_allowed: bool) {
        if !self.holds_viewer_files || self.rules(input_allowed).accept_files_from_viewer() {
            return;
        }
        let Some(link) = self.link.as_mut() else {
            return;
        };
        match link.withdraw_files() {
            Ok(()) => self.holds_viewer_files = false,
            Err(e) => self.report_failure(&e),
        }
    }

    /// Resolves when the host's clipboard changes (never, without a
    /// listener) or one of the settings does. Cancel-safe.
    pub(super) async fn changed(&mut self) -> ClipboardChange {
        let link = &mut self.link;
        let copied = async move {
            match link {
                Some(link) => link.changed().await,
                None => std::future::pending().await,
            }
        };
        let settings = &mut self.settings;
        tokio::select! {
            () = copied => ClipboardChange::Copied,
            Ok(()) = settings.sharing.changed() => ClipboardChange::Settings,
            Ok(()) = settings.receive_files.changed() => ClipboardChange::Settings,
        }
    }

    /// What to send the viewer now that the host's clipboard changed.
    /// Copied files are offered through `files`, which sends their list.
    pub(super) async fn local_change(
        &mut self,
        input_allowed: bool,
        files: &dyn FileSharing,
    ) -> Option<ControlMessage> {
        if !self.rules(input_allowed).send_to_viewer() {
            files.withdraw();
            return None;
        }
        let outcome = self.link.as_mut()?.read_outgoing().await;
        let outgoing = match outcome {
            Ok(outgoing) => outgoing,
            Err(e) => {
                self.report_failure(&e);
                return None;
            }
        };
        share_copied_files(&outgoing, files, self.files_not_shared());
        match outgoing {
            Outgoing::Send(text) => Some(ControlMessage::Clipboard(text)),
            Outgoing::TooLarge { first: true } => {
                self.events
                    .emit(HostEvent::Warning(HostWarning::ClipboardTooLarge));
                None
            }
            _ => None,
        }
    }

    /// The viewer sent `text`: puts it on the host's clipboard if the rules
    /// allow it.
    pub(super) async fn remote_text(&mut self, text: &str, input_allowed: bool) -> Received {
        if !self.rules(input_allowed).accept_from_viewer() {
            return Received::Ignored;
        }
        let Some(link) = self.link.as_mut() else {
            return Received::Ignored;
        };
        match link.receive(text).await {
            Ok(received) => received,
            Err(e) => {
                self.report_failure(&e);
                Received::Ignored
            }
        }
    }

    /// The viewer copied the files `entries`: they become pasteable here
    /// (fetched through `files` when pasted) if the rules allow it.
    pub(super) fn remote_files(
        &mut self,
        offer: OfferId,
        entries: Vec<FileEntry>,
        input_allowed: bool,
        files: &dyn FileSharing,
    ) -> Received {
        if !self.rules(input_allowed).accept_files_from_viewer() {
            return Received::Ignored;
        }
        let Some(link) = self.link.as_mut() else {
            return Received::Ignored;
        };
        match link.receive_files(offer, entries, files) {
            Ok(received) => {
                if received == Received::Applied {
                    self.holds_viewer_files = true;
                }
                received
            }
            Err(e) => {
                self.report_failure(&e);
                Received::Ignored
            }
        }
    }

    fn rules(&self, input_allowed: bool) -> HostRules {
        HostRules {
            sharing: *self.settings.sharing.borrow(),
            input_allowed,
            receive_files: *self.settings.receive_files.borrow(),
        }
    }

    /// Reports why copied files could not be offered.
    fn files_not_shared(&self) -> NotShared {
        let events = self.events.clone();
        Box::new(move |error| {
            events.emit(HostEvent::Warning(HostWarning::FilesNotShared(error)));
        })
    }

    fn report_failure(&mut self, error: &AccessError) {
        if self.failures.admit(Instant::now()).is_some() {
            self.events
                .emit(HostEvent::Warning(HostWarning::ClipboardFailed(
                    error.to_string(),
                )));
        }
    }
}

#[cfg(test)]
mod tests;
