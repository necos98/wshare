//! The host's side of the shared clipboard during one session: the text the
//! host copies goes to the viewer, and the viewer's text goes to the host's
//! clipboard, within the rules of [`HostRules`].
//!
//! A clipboard that cannot be used never ends the session: the problem is
//! reported as a warning and the session goes on without it.

use std::time::{Duration, Instant};

use protocol::control::ControlMessage;
use tokio::sync::watch;

use super::events::{EventSink, Throttle};
use super::{HostEvent, HostWarning};
use crate::clipboard::{
    AccessError, ClipboardAccess, ClipboardSync, HostRules, Outgoing, Received, SystemClipboard,
};

/// At most one warning about a failing clipboard this often: each change
/// the user makes or the viewer sends may fail the same way.
const FAILURE_WARNING_INTERVAL: Duration = Duration::from_secs(10);

pub(super) struct SessionClipboard<A = SystemClipboard> {
    /// `None` if the clipboard could not be listened to.
    link: Option<ClipboardSync<A>>,
    /// The "Condividi appunti" setting, read when it matters.
    sharing: watch::Receiver<bool>,
    events: EventSink,
    failures: Throttle,
}

impl SessionClipboard {
    /// Starts listening to the clipboard for this session.
    pub(super) fn open(sharing: watch::Receiver<bool>, events: EventSink) -> Self {
        let link = ClipboardSync::open();
        if let Err(e) = &link {
            events.emit(HostEvent::Warning(HostWarning::ClipboardUnavailable(
                format!("{e:#}"),
            )));
        }
        Self::with_link(link.ok(), sharing, events)
    }
}

impl<A: ClipboardAccess> SessionClipboard<A> {
    pub(super) fn with_link(
        link: Option<ClipboardSync<A>>,
        sharing: watch::Receiver<bool>,
        events: EventSink,
    ) -> Self {
        Self {
            link,
            sharing,
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
    pub(super) async fn local_change(&mut self, input_allowed: bool) -> Option<ControlMessage> {
        if !self.rules(input_allowed).send_to_viewer() {
            return None;
        }
        let outcome = self.link.as_mut()?.read_outgoing().await;
        match outcome {
            Ok(Outgoing::Send(text)) => Some(ControlMessage::Clipboard(text)),
            Ok(Outgoing::Skip) => None,
            Ok(Outgoing::TooLarge { first }) => {
                if first {
                    self.events
                        .emit(HostEvent::Warning(HostWarning::ClipboardTooLarge));
                }
                None
            }
            Err(e) => {
                self.report_failure(&e);
                None
            }
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

    fn rules(&self, input_allowed: bool) -> HostRules {
        HostRules {
            sharing: *self.sharing.borrow(),
            input_allowed,
        }
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
mod tests {
    use std::sync::mpsc::Receiver;

    use protocol::control::MAX_CLIPBOARD_BYTES;
    use tokio::sync::mpsc::UnboundedSender;

    use super::*;
    use crate::clipboard::ClipboardContent;
    use crate::clipboard::fake::{FakeClipboard, link};
    use crate::host::events::recording::recorder;

    struct Fixture {
        clipboard: SessionClipboard<FakeClipboard>,
        copied: FakeClipboard,
        _announce: UnboundedSender<()>,
        sharing: watch::Sender<bool>,
        events: Receiver<HostEvent>,
    }

    fn fixture(sharing: bool) -> Fixture {
        let (link, copied, announce) = link();
        let (sharing_tx, sharing_rx) = watch::channel(sharing);
        let (observer, events) = recorder();
        Fixture {
            clipboard: SessionClipboard::with_link(
                Some(link),
                sharing_rx,
                EventSink::new(observer),
            ),
            copied,
            _announce: announce,
            sharing: sharing_tx,
            events,
        }
    }

    fn copy(fixture: &Fixture, text: &str) {
        fixture.copied.0.borrow_mut().content = Some(ClipboardContent::Text(text.into()));
    }

    fn written(fixture: &Fixture) -> Vec<String> {
        fixture.copied.0.borrow().written.clone()
    }

    #[tokio::test]
    async fn text_copied_on_the_host_goes_to_the_viewer() {
        // Remote control does not matter for what the host sends.
        for input_allowed in [true, false] {
            let mut f = fixture(true);
            copy(&f, "ciao");
            assert_eq!(
                f.clipboard.local_change(input_allowed).await,
                Some(ControlMessage::Clipboard("ciao".into()))
            );
        }
    }

    #[tokio::test]
    async fn nothing_is_sent_while_the_setting_is_off() {
        let mut f = fixture(false);
        copy(&f, "segreto");
        assert_eq!(f.clipboard.local_change(true).await, None);
        // Turned on mid-session: the next change goes out.
        f.sharing.send(true).unwrap();
        assert_eq!(
            f.clipboard.local_change(true).await,
            Some(ControlMessage::Clipboard("segreto".into()))
        );
    }

    #[tokio::test]
    async fn the_viewers_text_needs_the_setting_and_remote_control() {
        let mut f = fixture(true);
        assert_eq!(
            f.clipboard.remote_text("no", false).await,
            Received::Ignored
        );
        f.sharing.send(false).unwrap();
        assert_eq!(f.clipboard.remote_text("no", true).await, Received::Ignored);
        assert!(written(&f).is_empty());

        f.sharing.send(true).unwrap();
        assert_eq!(f.clipboard.remote_text("sì", true).await, Received::Applied);
        assert_eq!(written(&f), ["sì"]);
    }

    #[tokio::test]
    async fn the_viewers_text_does_not_come_back() {
        let mut f = fixture(true);
        f.clipboard.remote_text("da remoto", true).await;
        assert_eq!(f.clipboard.local_change(true).await, None);
    }

    #[tokio::test]
    async fn text_too_long_is_warned_about_once_and_not_sent() {
        let mut f = fixture(true);
        f.copied.0.borrow_mut().content = Some(ClipboardContent::TooLarge);
        assert_eq!(f.clipboard.local_change(true).await, None);
        assert_eq!(f.clipboard.local_change(true).await, None);
        assert_eq!(
            f.events.try_recv(),
            Ok(HostEvent::Warning(HostWarning::ClipboardTooLarge))
        );
        assert!(f.events.try_recv().is_err(), "only once");
    }

    #[tokio::test]
    async fn too_long_text_from_the_viewer_is_ignored() {
        let mut f = fixture(true);
        let text = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        assert_eq!(
            f.clipboard.remote_text(&text, true).await,
            Received::Ignored
        );
        assert!(written(&f).is_empty());
    }

    #[tokio::test]
    async fn a_failing_clipboard_is_a_warning_not_an_error() {
        let mut f = fixture(true);
        f.copied.0.borrow_mut().fail_writes = true;
        assert_eq!(f.clipboard.remote_text("a", true).await, Received::Ignored);
        assert_eq!(f.clipboard.remote_text("b", true).await, Received::Ignored);
        assert_eq!(
            f.events.try_recv(),
            Ok(HostEvent::Warning(HostWarning::ClipboardFailed(
                "denied".into()
            )))
        );
        assert!(f.events.try_recv().is_err(), "throttled");
    }

    #[tokio::test]
    async fn without_a_listener_nothing_happens() {
        let (observer, _events) = recorder();
        let (_sharing_tx, sharing) = watch::channel(true);
        let mut clipboard =
            SessionClipboard::<FakeClipboard>::with_link(None, sharing, EventSink::new(observer));
        assert_eq!(clipboard.local_change(true).await, None);
        assert_eq!(clipboard.remote_text("a", true).await, Received::Ignored);
        let waiting = tokio::time::timeout(Duration::from_millis(20), clipboard.changed());
        assert!(waiting.await.is_err());
    }
}
