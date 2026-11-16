//! Pure rules of the shared clipboard: who may move text and files in which
//! direction, which text is worth sending, and what keeps a text from
//! bouncing between the two PCs forever.

use std::path::PathBuf;

use protocol::control::MAX_CLIPBOARD_BYTES;

/// What the local clipboard holds, as far as sharing is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardContent {
    Text(String),
    /// Files and folders copied in Explorer (`CF_HDROP`).
    Files(Vec<PathBuf>),
    /// Something else (an image, the other PC's files) or nothing: never
    /// shared.
    NoText,
    /// The program that copied it asked not to be monitored (password
    /// managers do): it stays on this PC.
    Private,
    /// Text longer than [`MAX_CLIPBOARD_BYTES`].
    TooLarge,
}

/// What to do after the local clipboard changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    Send(String),
    /// Offer these files: the other PC can paste them.
    SendFiles(Vec<PathBuf>),
    /// The files offered last are still what is copied: nothing new.
    Unchanged,
    /// Nothing to send: not text, private, empty, or the text the other PC
    /// just gave us.
    Skip,
    /// The text is too long to send. `first` is true only for the first
    /// such text of the session: that one is worth a warning.
    TooLarge {
        first: bool,
    },
}

/// What the host's side of a session allows right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostRules {
    /// The "Condividi appunti" setting.
    pub sharing: bool,
    /// Whether the viewer may control this PC.
    pub input_allowed: bool,
    /// The "Consenti ricezione file" setting.
    pub receive_files: bool,
}

impl HostRules {
    /// The host's copied text goes to the viewer.
    pub fn send_to_viewer(self) -> bool {
        self.sharing
    }

    /// The viewer's text goes to the host's clipboard: pasting it is a way
    /// of typing on this PC, so it needs the same permission as remote
    /// control, and the setting that turns the clipboard off.
    pub fn accept_from_viewer(self) -> bool {
        self.sharing && self.input_allowed
    }

    /// The files the host copies can be pasted on the viewer: the same
    /// rule as for text.
    pub fn send_files_to_viewer(self) -> bool {
        self.send_to_viewer()
    }

    /// The files the viewer copies can be pasted here: as for text, and
    /// the host must accept files at all.
    pub fn accept_files_from_viewer(self) -> bool {
        self.accept_from_viewer() && self.receive_files
    }

    /// The viewer may drop files on its window or send them: they arrive
    /// in this PC's download folder. It needs remote control (receiving
    /// files is a way of acting on this PC) and the setting, not the
    /// clipboard.
    pub fn accept_pushed_files(self) -> bool {
        self.input_allowed && self.receive_files
    }
}

/// What the viewer's side of a session allows right now. The host's
/// "Condividi appunti" setting is not known here: a host that has it off
/// ignores what arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerRules {
    /// The host allows remote control.
    pub host_allows: bool,
    /// The viewer was opened view-only.
    pub view_only: bool,
}

impl ViewerRules {
    /// The viewer's copied text goes to the host.
    pub fn send_to_host(self) -> bool {
        self.host_allows && !self.view_only
    }

    /// The viewer's copied or dropped files go to the host (which may still
    /// refuse them, see [`HostRules`]).
    pub fn send_files_to_host(self) -> bool {
        self.send_to_host()
    }
}

/// Remembers the text last moved between the two clipboards, in either
/// direction.
///
/// Writing the clipboard makes it "change" on the writer too: without this
/// memory that echo would be sent back, written again, and so on forever.
/// Copying the same text twice in a row sends it once; so are the same
/// files, while nothing else was copied in between.
#[derive(Debug, Default)]
pub struct ClipboardGuard {
    last: Option<String>,
    last_files: Option<Vec<PathBuf>>,
    warned_too_large: bool,
}

impl ClipboardGuard {
    /// Decides what to do with the clipboard as it was just read.
    pub fn outgoing(&mut self, content: ClipboardContent) -> Outgoing {
        let ClipboardContent::Files(files) = content else {
            self.last_files = None;
            return self.outgoing_text(content);
        };
        if files.is_empty() {
            return Outgoing::Skip;
        }
        if self.last_files.as_ref() == Some(&files) {
            return Outgoing::Unchanged;
        }
        self.last_files = Some(files.clone());
        Outgoing::SendFiles(files)
    }

    fn outgoing_text(&mut self, content: ClipboardContent) -> Outgoing {
        match content {
            ClipboardContent::Files(_) => Outgoing::Skip,
            ClipboardContent::NoText | ClipboardContent::Private => Outgoing::Skip,
            ClipboardContent::TooLarge => self.too_large(),
            ClipboardContent::Text(text) if text.len() > MAX_CLIPBOARD_BYTES => self.too_large(),
            ClipboardContent::Text(text) if text.is_empty() || self.is_last(&text) => {
                Outgoing::Skip
            }
            ClipboardContent::Text(text) => {
                self.last = Some(text.clone());
                Outgoing::Send(text)
            }
        }
    }

    /// The text to write for what the other PC sent, `None` if it must be
    /// ignored: empty, too long, or the one the clipboard already holds.
    /// Text after a NUL is cut off: the clipboard's text ends there.
    pub fn incoming(&self, text: &str) -> Option<String> {
        let text = text.split('\0').next().unwrap_or_default();
        let acceptable = !text.is_empty() && text.len() <= MAX_CLIPBOARD_BYTES;
        (acceptable && !self.is_last(text)).then(|| text.to_string())
    }

    /// `text` was written to the local clipboard: its change is an echo.
    pub fn applied(&mut self, text: String) {
        self.last = Some(text);
    }

    fn is_last(&self, text: &str) -> bool {
        self.last.as_deref() == Some(text)
    }

    fn too_large(&mut self) -> Outgoing {
        let first = !std::mem::replace(&mut self.warned_too_large, true);
        Outgoing::TooLarge { first }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> ClipboardContent {
        ClipboardContent::Text(text.to_string())
    }

    #[test]
    fn new_text_is_sent() {
        let mut guard = ClipboardGuard::default();
        assert_eq!(guard.outgoing(text("ciao")), Outgoing::Send("ciao".into()));
    }

    #[test]
    fn the_same_text_twice_in_a_row_is_sent_once() {
        let mut guard = ClipboardGuard::default();
        guard.outgoing(text("ciao"));
        assert_eq!(guard.outgoing(text("ciao")), Outgoing::Skip);
        assert_eq!(
            guard.outgoing(text("altro")),
            Outgoing::Send("altro".into())
        );
        assert_eq!(guard.outgoing(text("ciao")), Outgoing::Send("ciao".into()));
    }

    #[test]
    fn what_the_other_pc_sent_is_not_sent_back() {
        let mut guard = ClipboardGuard::default();
        let written = guard.incoming("da remoto").expect("acceptable");
        guard.applied(written);
        assert_eq!(guard.outgoing(text("da remoto")), Outgoing::Skip);
        assert_eq!(guard.incoming("da remoto"), None, "nothing new to write");
    }

    #[test]
    fn the_echo_of_our_own_text_is_not_written_again() {
        let mut guard = ClipboardGuard::default();
        guard.outgoing(text("mio"));
        assert_eq!(guard.incoming("mio"), None);
    }

    #[test]
    fn nothing_bounces_between_two_guards() {
        // Two PCs, each with a guard: A copies, B receives and writes, B's
        // clipboard changes (the echo), and nothing travels back.
        let (mut a, mut b) = (ClipboardGuard::default(), ClipboardGuard::default());
        let Outgoing::Send(sent) = a.outgoing(text("ciao")) else {
            panic!("A sends");
        };
        let written = b.incoming(&sent).expect("B writes");
        b.applied(written);
        assert_eq!(b.outgoing(text("ciao")), Outgoing::Skip);
        assert_eq!(a.outgoing(text("ciao")), Outgoing::Skip);
    }

    #[test]
    fn other_content_is_never_sent() {
        let mut guard = ClipboardGuard::default();
        for content in [
            ClipboardContent::NoText,
            ClipboardContent::Private,
            text(""),
        ] {
            assert_eq!(guard.outgoing(content), Outgoing::Skip);
        }
    }

    #[test]
    fn the_size_limit_is_in_utf8_bytes_and_warns_once() {
        let mut guard = ClipboardGuard::default();
        let longest = "x".repeat(MAX_CLIPBOARD_BYTES);
        assert!(matches!(guard.outgoing(text(&longest)), Outgoing::Send(_)));

        // 'è' takes two bytes: half the limit in characters is not enough.
        let too_long = "è".repeat(MAX_CLIPBOARD_BYTES / 2 + 1);
        assert_eq!(
            guard.outgoing(text(&too_long)),
            Outgoing::TooLarge { first: true }
        );
        assert_eq!(
            guard.outgoing(ClipboardContent::TooLarge),
            Outgoing::TooLarge { first: false }
        );
        assert_eq!(
            guard.outgoing(text(&"y".repeat(MAX_CLIPBOARD_BYTES + 1))),
            Outgoing::TooLarge { first: false }
        );
    }

    #[test]
    fn a_text_too_large_is_not_remembered_as_the_last_one() {
        let mut guard = ClipboardGuard::default();
        guard.outgoing(text("breve"));
        guard.outgoing(ClipboardContent::TooLarge);
        assert_eq!(guard.outgoing(text("breve")), Outgoing::Skip);
    }

    #[test]
    fn incoming_text_is_checked() {
        let guard = ClipboardGuard::default();
        assert_eq!(guard.incoming("ciao"), Some("ciao".into()));
        assert_eq!(guard.incoming(""), None);
        assert_eq!(guard.incoming("\0nascosto"), None);
        assert_eq!(guard.incoming("uno\0due"), Some("uno".into()));
        let longest = "x".repeat(MAX_CLIPBOARD_BYTES);
        assert_eq!(guard.incoming(&longest), Some(longest));
        assert_eq!(guard.incoming(&"x".repeat(MAX_CLIPBOARD_BYTES + 1)), None);
    }

    fn files(names: &[&str]) -> ClipboardContent {
        ClipboardContent::Files(names.iter().map(PathBuf::from).collect())
    }

    #[test]
    fn copied_files_are_offered_once_until_something_else_is_copied() {
        let mut guard = ClipboardGuard::default();
        let offered = Outgoing::SendFiles(vec![PathBuf::from("C:\\a.txt")]);
        assert_eq!(guard.outgoing(files(&["C:\\a.txt"])), offered);
        assert_eq!(guard.outgoing(files(&["C:\\a.txt"])), Outgoing::Unchanged);
        assert_eq!(
            guard.outgoing(files(&["C:\\b.txt"])),
            Outgoing::SendFiles(vec![PathBuf::from("C:\\b.txt")])
        );
        // The other PC's files were put here meanwhile: copying the same
        // files again offers them again.
        assert_eq!(guard.outgoing(ClipboardContent::NoText), Outgoing::Skip);
        assert_eq!(
            guard.outgoing(files(&["C:\\b.txt"])),
            Outgoing::SendFiles(vec![PathBuf::from("C:\\b.txt")])
        );
        assert_eq!(guard.outgoing(files(&[])), Outgoing::Skip);
    }

    #[test]
    fn text_between_two_copies_of_files_counts_as_something_else() {
        let mut guard = ClipboardGuard::default();
        guard.outgoing(files(&["C:\\a"]));
        assert_eq!(guard.outgoing(text("ciao")), Outgoing::Send("ciao".into()));
        assert!(matches!(
            guard.outgoing(files(&["C:\\a"])),
            Outgoing::SendFiles(_)
        ));
        // And the text is still remembered against its own echo.
        assert_eq!(guard.incoming("ciao"), None);
    }

    fn host(sharing: bool, input_allowed: bool, receive_files: bool) -> HostRules {
        HostRules {
            sharing,
            input_allowed,
            receive_files,
        }
    }

    #[test]
    fn the_hosts_copied_files_follow_the_clipboard_setting_only() {
        for (input, receive) in [(true, true), (false, false), (true, false)] {
            assert!(host(true, input, receive).send_files_to_viewer());
            assert!(!host(false, input, receive).send_files_to_viewer());
        }
    }

    #[test]
    fn the_viewers_copied_files_need_clipboard_control_and_reception() {
        assert!(host(true, true, true).accept_files_from_viewer());
        assert!(
            !host(false, true, true).accept_files_from_viewer(),
            "clipboard off"
        );
        assert!(
            !host(true, false, true).accept_files_from_viewer(),
            "control off"
        );
        assert!(
            !host(true, true, false).accept_files_from_viewer(),
            "reception off"
        );
    }

    #[test]
    fn pushed_files_need_control_and_reception_but_not_the_clipboard() {
        assert!(host(false, true, true).accept_pushed_files());
        assert!(host(true, true, true).accept_pushed_files());
        assert!(
            !host(true, false, true).accept_pushed_files(),
            "control off"
        );
        assert!(
            !host(true, true, false).accept_pushed_files(),
            "reception off"
        );
    }

    #[test]
    fn the_viewer_sends_files_only_when_it_may_control_the_host() {
        let sends = |host_allows, view_only| {
            ViewerRules {
                host_allows,
                view_only,
            }
            .send_files_to_host()
        };
        assert!(sends(true, false));
        assert!(!sends(true, true), "view only");
        assert!(!sends(false, false));
    }

    #[test]
    fn the_host_sends_only_while_sharing() {
        for input_allowed in [true, false] {
            let rules = |sharing| HostRules {
                sharing,
                input_allowed,
                receive_files: true,
            };
            assert!(rules(true).send_to_viewer());
            assert!(!rules(false).send_to_viewer());
        }
    }

    #[test]
    fn the_host_accepts_text_only_with_sharing_and_remote_control() {
        let accepts = |sharing, input_allowed| {
            HostRules {
                sharing,
                input_allowed,
                receive_files: true,
            }
            .accept_from_viewer()
        };
        assert!(accepts(true, true));
        assert!(!accepts(true, false), "remote control is off");
        assert!(!accepts(false, true), "the clipboard is not shared");
        assert!(!accepts(false, false));
    }

    #[test]
    fn the_viewer_sends_only_when_it_may_control_the_host() {
        let sends = |host_allows, view_only| {
            ViewerRules {
                host_allows,
                view_only,
            }
            .send_to_host()
        };
        assert!(sends(true, false));
        assert!(!sends(true, true), "view only");
        assert!(!sends(false, false), "the host forbids control");
        assert!(!sends(false, true));
    }
}
