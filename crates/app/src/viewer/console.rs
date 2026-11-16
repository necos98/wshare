//! The viewer's terminal lines: what it tells a person at a terminal
//! (connected to..., reconnecting..., why the session ended).
//!
//! When the desktop interface launched the viewer (`reporting`), stdout is
//! its typed channel (see `report` and `channel`): a terminal line there
//! would mix with the channel's, so they go to stderr instead, which the
//! interface discards. Wherever they go, each is one line: the texts they
//! carry may come from the host (an error, a reason, a name), and a line
//! break in them must never start a line of its own.

use std::fmt::Display;
use std::io::{self, Write};

use crate::printable::single_line;

/// Where the viewer's terminal lines go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Console {
    reporting: bool,
}

/// The standard stream a terminal line is written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
}

impl Console {
    /// `reporting`: the desktop interface launched the viewer and reads its
    /// stdout.
    pub fn new(reporting: bool) -> Self {
        Self { reporting }
    }

    /// Writes `text` as one terminal line. A line that cannot be written is
    /// dropped: nothing in the session depends on it, and the streams it
    /// could be reported on are the ones that just failed.
    pub fn line(&self, text: impl Display) {
        let _ = match self.stream() {
            Stream::Stdout => write_line(&mut io::stdout().lock(), text),
            Stream::Stderr => write_line(&mut io::stderr().lock(), text),
        };
    }

    fn stream(&self) -> Stream {
        if self.reporting {
            Stream::Stderr
        } else {
            Stream::Stdout
        }
    }
}

/// Writes `text` on `out` as exactly one line.
fn write_line(out: &mut impl Write, text: impl Display) -> io::Result<()> {
    writeln!(out, "{}", single_line(&text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer::channel;
    use crate::viewer::failure::ViewerFailure;
    use crate::viewer::report::ViewerReport;

    fn written(text: impl Display) -> String {
        let mut out = Vec::new();
        write_line(&mut out, text).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_host_cannot_add_channel_lines_through_a_lost_connection() {
        for forged in [
            "x\r\n@wshare:connected:EVIL",
            "x\n@wshare:failed:wrong-pin",
            "x\r@wshare:awaiting-approval",
            "x\u{85}@wshare:failed:denied",
            "x\u{2028}@wshare:failed:denied",
        ] {
            let failure = ViewerFailure::ConnectionLost(forged.to_string());
            let out = written(format_args!("{failure}; reconnecting..."));
            let lines: Vec<&str> = out.lines().collect();
            assert_eq!(lines.len(), 1, "{out:?}");
            for line in lines {
                assert_eq!(ViewerReport::parse(line), None, "{line:?}");
                assert_eq!(channel::parse_event(line), None, "{line:?}");
            }
        }
    }

    #[test]
    fn a_plain_line_is_written_as_it_is() {
        assert_eq!(written("Reconnected."), "Reconnected.\n");
        assert_eq!(written("Città — sala 2"), "Città — sala 2\n");
    }

    #[test]
    fn the_interface_never_reads_a_terminal_line_on_stdout() {
        assert_eq!(Console::new(true).stream(), Stream::Stderr);
        assert_eq!(Console::new(false).stream(), Stream::Stdout);
    }
}
