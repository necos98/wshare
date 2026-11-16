//! One session's clipboard link: waits for the local clipboard to change,
//! decides what is worth sending, and writes what the other PC sent.

use anyhow::Result;
use tokio::sync::mpsc::UnboundedReceiver;

use super::access::{AccessError, ClipboardAccess, Retry};
use super::rules::{ClipboardGuard, Outgoing};
use protocol::transfer::{FileEntry, OfferId};

use super::file_sharing::FileSharing;
use super::system::SystemClipboard;
use super::virtual_files;
use super::window::ClipboardWindow;

/// What became of text that arrived from the other PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    Applied,
    /// Not written: empty, too long, or what the clipboard already holds.
    Ignored,
}

pub struct ClipboardSync<A> {
    access: A,
    guard: ClipboardGuard,
    retry: Retry,
    /// One message per change of the local clipboard.
    changes: UnboundedReceiver<()>,
    /// Keeps the window that listens for changes open.
    _window: Option<ClipboardWindow>,
}

impl ClipboardSync<SystemClipboard> {
    /// Starts listening to the real clipboard; it stops when this is
    /// dropped.
    pub fn open() -> Result<Self> {
        let (window, changes) = ClipboardWindow::open()?;
        let access = SystemClipboard::owned_by(&window);
        Ok(Self {
            access,
            guard: ClipboardGuard::default(),
            retry: Retry::default(),
            changes,
            _window: Some(window),
        })
    }
}

impl<A: ClipboardAccess> ClipboardSync<A> {
    /// A link on `access`, whose changes arrive on `changes`.
    #[cfg(test)]
    pub fn with_access(access: A, changes: UnboundedReceiver<()>, retry: Retry) -> Self {
        Self {
            access,
            guard: ClipboardGuard::default(),
            retry,
            changes,
            _window: None,
        }
    }

    /// Resolves when the local clipboard changes. Never resolves if the
    /// listener is gone: sharing then just stops. Safe to drop and call
    /// again.
    pub async fn changed(&mut self) {
        if self.changes.recv().await.is_none() {
            std::future::pending::<()>().await;
        }
    }

    /// Reads the clipboard after a change and says what to do about it.
    /// Changes that piled up meanwhile are covered by this one read.
    pub async fn read_outgoing(&mut self) -> Result<Outgoing, AccessError> {
        while self.changes.try_recv().is_ok() {}
        let content = self.retry.run(|| self.access.read()).await?;
        Ok(self.guard.outgoing(content))
    }

    /// Puts the text the other PC sent on the clipboard.
    pub async fn receive(&mut self, text: &str) -> Result<Received, AccessError> {
        let Some(text) = self.guard.incoming(text) else {
            return Ok(Received::Ignored);
        };
        self.retry.run(|| self.access.write(&text)).await?;
        self.guard.applied(text);
        Ok(Received::Applied)
    }

    /// Puts the other PC's copied files `entries` on the clipboard, if their
    /// list passes the checks (an unacceptable one is ignored). Writing
    /// them makes the clipboard change here too; that change reads as
    /// nothing to share (they are not `CF_HDROP`), so nothing bounces back.
    pub fn receive_files(
        &mut self,
        offer: OfferId,
        entries: Vec<FileEntry>,
        sharing: &dyn FileSharing,
    ) -> Result<Received, AccessError> {
        let Ok(files) = virtual_files::prepare(offer, entries, sharing.paste_link()) else {
            return Ok(Received::Ignored);
        };
        self.access.offer_files(files)?;
        Ok(Received::Applied)
    }
}

/// The files side of what the clipboard now holds: copied files are
/// offered to the other PC (`on_error` hears why they cannot be); anything
/// else withdraws the files offered before. The same files still copied
/// change nothing.
pub fn share_copied_files(
    outgoing: &Outgoing,
    sharing: &dyn FileSharing,
    on_error: Box<dyn FnOnce(String) + Send>,
) {
    match outgoing {
        Outgoing::SendFiles(picked) => sharing.offer(picked.clone(), on_error),
        Outgoing::Unchanged => {}
        _ => sharing.withdraw(),
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! An in-memory clipboard for tests of the code that uses the link.

    use std::cell::RefCell;
    use std::rc::Rc;

    use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

    use super::super::access::{AccessError, ClipboardAccess, Retry};
    use super::super::rules::ClipboardContent;
    use super::super::virtual_files::RemoteFiles;
    use super::ClipboardSync;

    #[derive(Default)]
    pub struct Contents {
        pub content: Option<ClipboardContent>,
        pub busy_reads: u32,
        pub busy_writes: u32,
        pub fail_writes: bool,
        pub written: Vec<String>,
        /// The paths of each list of files put on the clipboard.
        pub offered: Vec<Vec<String>>,
    }

    #[derive(Clone, Default)]
    pub struct FakeClipboard(pub Rc<RefCell<Contents>>);

    impl ClipboardAccess for FakeClipboard {
        fn read(&self) -> Result<ClipboardContent, AccessError> {
            let mut contents = self.0.borrow_mut();
            if contents.busy_reads > 0 {
                contents.busy_reads -= 1;
                return Err(AccessError::Busy);
            }
            Ok(contents.content.clone().unwrap_or(ClipboardContent::NoText))
        }

        fn write(&self, text: &str) -> Result<(), AccessError> {
            let mut contents = self.0.borrow_mut();
            if contents.fail_writes {
                return Err(AccessError::Failed("denied".into()));
            }
            if contents.busy_writes > 0 {
                contents.busy_writes -= 1;
                return Err(AccessError::Busy);
            }
            contents.written.push(text.to_string());
            contents.content = Some(ClipboardContent::Text(text.to_string()));
            Ok(())
        }

        fn offer_files(&self, files: RemoteFiles) -> Result<(), AccessError> {
            let mut contents = self.0.borrow_mut();
            if contents.fail_writes {
                return Err(AccessError::Failed("denied".into()));
            }
            let paths = files.entries.into_iter().map(|e| e.path).collect();
            contents.offered.push(paths);
            contents.content = Some(ClipboardContent::NoText);
            Ok(())
        }
    }

    pub const QUICK: Retry = Retry {
        attempts: 3,
        delay: std::time::Duration::ZERO,
    };

    /// A link on a fake clipboard, with the handle to change its contents
    /// and to announce a change.
    pub fn link() -> (
        ClipboardSync<FakeClipboard>,
        FakeClipboard,
        UnboundedSender<()>,
    ) {
        let clipboard = FakeClipboard::default();
        let (announce, changes) = unbounded_channel();
        let link = ClipboardSync::with_access(clipboard.clone(), changes, QUICK);
        (link, clipboard, announce)
    }
}

#[cfg(test)]
mod tests {
    use super::fake::link;
    use super::*;
    use crate::clipboard::ClipboardContent;

    fn copied(clipboard: &super::fake::FakeClipboard, text: &str) {
        clipboard.0.borrow_mut().content = Some(ClipboardContent::Text(text.into()));
    }

    #[tokio::test]
    async fn a_change_makes_the_new_text_go_out() {
        let (mut link, clipboard, announce) = link();
        copied(&clipboard, "ciao");
        announce.send(()).unwrap();
        link.changed().await;
        assert_eq!(
            link.read_outgoing().await,
            Ok(Outgoing::Send("ciao".into()))
        );
    }

    #[tokio::test]
    async fn changes_that_piled_up_are_read_once() {
        let (mut link, clipboard, announce) = link();
        copied(&clipboard, "ultimo");
        for _ in 0..3 {
            announce.send(()).unwrap();
        }
        link.changed().await;
        assert_eq!(
            link.read_outgoing().await,
            Ok(Outgoing::Send("ultimo".into()))
        );
        // Both extra announcements were covered: nothing is waiting.
        let waiting = tokio::time::timeout(std::time::Duration::from_millis(20), link.changed());
        assert!(waiting.await.is_err());
    }

    #[tokio::test]
    async fn without_a_listener_the_link_just_stays_quiet() {
        let (mut link, _clipboard, announce) = link();
        drop(announce);
        let waiting = tokio::time::timeout(std::time::Duration::from_millis(20), link.changed());
        assert!(waiting.await.is_err());
    }

    #[tokio::test]
    async fn received_text_is_written_and_its_echo_is_not_sent_back() {
        let (mut link, clipboard, _announce) = link();
        assert_eq!(link.receive("da remoto").await, Ok(Received::Applied));
        assert_eq!(clipboard.0.borrow().written, ["da remoto"]);
        // The write made the local clipboard change; reading it finds the
        // text we just wrote.
        assert_eq!(link.read_outgoing().await, Ok(Outgoing::Skip));
        assert_eq!(link.receive("da remoto").await, Ok(Received::Ignored));
        assert_eq!(clipboard.0.borrow().written.len(), 1);
    }

    #[tokio::test]
    async fn unacceptable_text_is_not_written() {
        let (mut link, clipboard, _announce) = link();
        assert_eq!(link.receive("").await, Ok(Received::Ignored));
        assert_eq!(link.receive("\0").await, Ok(Received::Ignored));
        let too_long = "x".repeat(protocol::control::MAX_CLIPBOARD_BYTES + 1);
        assert_eq!(link.receive(&too_long).await, Ok(Received::Ignored));
        assert!(clipboard.0.borrow().written.is_empty());
    }

    #[tokio::test]
    async fn a_busy_clipboard_is_retried_for_reading_and_writing() {
        let (mut link, clipboard, _announce) = link();
        copied(&clipboard, "ciao");
        clipboard.0.borrow_mut().busy_reads = 1;
        assert_eq!(
            link.read_outgoing().await,
            Ok(Outgoing::Send("ciao".into()))
        );
        clipboard.0.borrow_mut().busy_writes = 2;
        assert_eq!(link.receive("altro").await, Ok(Received::Applied));
    }

    #[tokio::test]
    async fn a_clipboard_that_never_frees_up_is_an_error_not_a_crash() {
        let (mut link, clipboard, _announce) = link();
        clipboard.0.borrow_mut().busy_writes = 10;
        assert_eq!(link.receive("ciao").await, Err(AccessError::Busy));
        // The failed write must not make its text look like an echo.
        clipboard.0.borrow_mut().busy_writes = 0;
        assert_eq!(link.receive("ciao").await, Ok(Received::Applied));
    }

    #[tokio::test]
    async fn text_too_large_is_reported_once() {
        let (mut link, clipboard, _announce) = link();
        clipboard.0.borrow_mut().content = Some(ClipboardContent::TooLarge);
        assert_eq!(
            link.read_outgoing().await,
            Ok(Outgoing::TooLarge { first: true })
        );
        assert_eq!(
            link.read_outgoing().await,
            Ok(Outgoing::TooLarge { first: false })
        );
    }
}
