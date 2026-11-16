//! The viewer's side of the shared clipboard: the text copied here goes to
//! the host when this viewer may control it, and the host's text goes to
//! this clipboard.
//!
//! Problems never end the session: each kind is said once on the terminal
//! (`wshare view` prints its warnings there) and the session goes on.

use protocol::control::{ControlMessage, MAX_CLIPBOARD_BYTES};

use crate::clipboard::{
    AccessError, ClipboardAccess, ClipboardSync, Outgoing, Received, SystemClipboard, ViewerRules,
};

pub struct ViewerClipboard<A = SystemClipboard> {
    /// `None` if the clipboard could not be listened to.
    link: Option<ClipboardSync<A>>,
    rules: ViewerRules,
    failure_reported: bool,
}

impl ViewerClipboard {
    /// Starts listening to the clipboard for this session.
    pub fn open(rules: ViewerRules) -> Self {
        let link = ClipboardSync::open()
            .inspect_err(|e| eprintln!("warning: {e:#}; the clipboard is not shared"))
            .ok();
        Self::with_link(link, rules)
    }
}

impl<A: ClipboardAccess> ViewerClipboard<A> {
    pub fn with_link(link: Option<ClipboardSync<A>>, rules: ViewerRules) -> Self {
        Self {
            link,
            rules,
            failure_reported: false,
        }
    }

    /// The host allowed or forbade remote control.
    pub fn set_host_allows(&mut self, allowed: bool) {
        self.rules.host_allows = allowed;
    }

    /// Resolves when the local clipboard changes (never, without a
    /// listener).
    pub async fn changed(&mut self) {
        match &mut self.link {
            Some(link) => link.changed().await,
            None => std::future::pending().await,
        }
    }

    /// What to send the host now that the local clipboard changed.
    pub async fn local_change(&mut self) -> Option<ControlMessage> {
        if !self.rules.send_to_host() {
            return None;
        }
        match self.link.as_mut()?.read_outgoing().await {
            Ok(Outgoing::Send(text)) => Some(ControlMessage::Clipboard(text)),
            Ok(Outgoing::Skip) => None,
            Ok(Outgoing::TooLarge { first }) => {
                if first {
                    eprintln!(
                        "warning: the copied text is too long to share (over {} KiB)",
                        MAX_CLIPBOARD_BYTES / 1024
                    );
                }
                None
            }
            Err(e) => {
                self.report_failure(&e);
                None
            }
        }
    }

    /// The host sent `text`: puts it on this clipboard.
    pub async fn remote_text(&mut self, text: &str) -> Received {
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

    fn report_failure(&mut self, error: &AccessError) {
        if !std::mem::replace(&mut self.failure_reported, true) {
            eprintln!("warning: clipboard sharing failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::ClipboardContent;
    use crate::clipboard::fake::{FakeClipboard, link};

    fn viewer(
        host_allows: bool,
        view_only: bool,
    ) -> (ViewerClipboard<FakeClipboard>, FakeClipboard) {
        // Reads are driven by the tests, not by change announcements.
        let (link, copied, _announce) = link();
        let rules = ViewerRules {
            host_allows,
            view_only,
        };
        (ViewerClipboard::with_link(Some(link), rules), copied)
    }

    fn copy(clipboard: &FakeClipboard, text: &str) {
        clipboard.0.borrow_mut().content = Some(ClipboardContent::Text(text.into()));
    }

    #[tokio::test]
    async fn text_copied_here_goes_to_a_host_that_allows_control() {
        let (mut viewer, copied) = viewer(true, false);
        copy(&copied, "ciao");
        assert_eq!(
            viewer.local_change().await,
            Some(ControlMessage::Clipboard("ciao".into()))
        );
    }

    #[tokio::test]
    async fn a_view_only_viewer_sends_nothing() {
        let (mut viewer, copied) = viewer(true, true);
        copy(&copied, "ciao");
        assert_eq!(viewer.local_change().await, None);
    }

    #[tokio::test]
    async fn nothing_is_sent_while_the_host_forbids_control() {
        let (mut viewer, copied) = viewer(false, false);
        copy(&copied, "ciao");
        assert_eq!(viewer.local_change().await, None);

        viewer.set_host_allows(true);
        assert_eq!(
            viewer.local_change().await,
            Some(ControlMessage::Clipboard("ciao".into()))
        );
        viewer.set_host_allows(false);
        copy(&copied, "altro");
        assert_eq!(viewer.local_change().await, None);
    }

    #[tokio::test]
    async fn the_hosts_text_arrives_even_in_view_only() {
        let (mut viewer, copied) = viewer(false, true);
        assert_eq!(viewer.remote_text("dal host").await, Received::Applied);
        assert_eq!(copied.0.borrow().written, ["dal host"]);
    }

    #[tokio::test]
    async fn the_hosts_text_does_not_come_back() {
        let (mut viewer, _copied) = viewer(true, false);
        viewer.remote_text("dal host").await;
        assert_eq!(viewer.local_change().await, None);
    }

    #[tokio::test]
    async fn too_long_text_from_the_host_is_ignored() {
        let (mut viewer, copied) = viewer(true, false);
        let text = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        assert_eq!(viewer.remote_text(&text).await, Received::Ignored);
        assert!(copied.0.borrow().written.is_empty());
    }

    #[tokio::test]
    async fn a_failing_clipboard_does_not_stop_the_session() {
        let (mut viewer, copied) = viewer(true, false);
        copied.0.borrow_mut().fail_writes = true;
        assert_eq!(viewer.remote_text("a").await, Received::Ignored);
        assert_eq!(viewer.remote_text("b").await, Received::Ignored);
    }

    #[tokio::test]
    async fn without_a_listener_nothing_happens() {
        let mut viewer = ViewerClipboard::<FakeClipboard>::with_link(
            None,
            ViewerRules {
                host_allows: true,
                view_only: false,
            },
        );
        assert_eq!(viewer.local_change().await, None);
        assert_eq!(viewer.remote_text("a").await, Received::Ignored);
    }
}
