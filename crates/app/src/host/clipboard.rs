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
    AccessError, ClipboardAccess, ClipboardSync, FileSharing, HostRules, Outgoing, Received,
    SystemClipboard, share_copied_files,
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

pub(super) struct SessionClipboard<A = SystemClipboard> {
    /// `None` if the clipboard could not be listened to.
    link: Option<ClipboardSync<A>>,
    settings: ClipboardSettings,
    events: EventSink,
    failures: Throttle,
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
        }
    }

    /// Resolves when the host's clipboard changes (never, without a
    /// listener).
    pub(super) async fn changed(&mut self) {
        match &mut self.link {
            Some(link) => link.changed().await,
            None => std::future::pending().await,
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
            Ok(received) => received,
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
    fn files_not_shared(&self) -> Box<dyn FnOnce(String) + Send> {
        let events = self.events.clone();
        Box::new(move |detail| {
            events.emit(HostEvent::Warning(HostWarning::FilesNotShared(detail)));
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
