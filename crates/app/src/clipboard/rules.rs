//! Pure rules of the shared clipboard: who may move text in which
//! direction, which text is worth sending, and what keeps a text from
//! bouncing between the two PCs forever.

use protocol::control::MAX_CLIPBOARD_BYTES;

/// What the local clipboard holds, as far as sharing is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardContent {
    Text(String),
    /// Something else (an image, files) or nothing: never shared.
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
}

/// Remembers the text last moved between the two clipboards, in either
/// direction.
///
/// Writing the clipboard makes it "change" on the writer too: without this
/// memory that echo would be sent back, written again, and so on forever.
/// Copying the same text twice in a row sends it once.
#[derive(Debug, Default)]
pub struct ClipboardGuard {
    last: Option<String>,
    warned_too_large: bool,
}

impl ClipboardGuard {
    /// Decides what to do with the clipboard as it was just read.
    pub fn outgoing(&mut self, content: ClipboardContent) -> Outgoing {
        match content {
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

    #[test]
    fn the_host_sends_only_while_sharing() {
        for input_allowed in [true, false] {
            let rules = |sharing| HostRules {
                sharing,
                input_allowed,
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
