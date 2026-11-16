//! The file transfers' part of the channel between the desktop interface
//! and the viewers it launches: typed lines, like the status lines of
//! `report`.
//!
//! - viewer -> interface, on stdout (only when `report::REPORT_ENV` is
//!   set): every [`TransferEvent`] as one line,
//!   `@wshare:transfer-started:<id>:<s|r>:<push|paste>:<files>:<bytes>:<name>`,
//!   `@wshare:transfer-progress:<id>:<bytes done>` and
//!   `@wshare:transfer-ended:<id>:<outcome>[:<detail>]`;
//! - interface -> viewer, on stdin: [`TransferCommand`]s,
//!   `@wshare:send-files:<path>|<path>...` (`|` cannot be in a Windows
//!   path), `@wshare:cancel-transfer:<id>` and `@wshare:cancel-transfers`.
//!
//! Lines that are not of this shape are ignored both ways, so a terminal
//! user's typing or ordinary output never turns into a command or an
//! event. The viewer reads stdin only once connected and only when the
//! interface launched it: the PIN prompt of a terminal is never involved.

use std::io::BufRead;
use std::path::PathBuf;

use protocol::transfer::{TransferError, TransferId};
use tokio::sync::mpsc::UnboundedSender;

use super::report::{LINE_PREFIX, SEPARATOR};
use crate::printable::single_line;
use crate::transfer::{Direction, Kind, Outcome, TransferCommand, TransferEvent, TransferInfo};

const STARTED: &str = "transfer-started";
const PROGRESS: &str = "transfer-progress";
const ENDED: &str = "transfer-ended";
const SEND_FILES: &str = "send-files";
const CANCEL: &str = "cancel-transfer";
const CANCEL_ALL: &str = "cancel-transfers";
/// Between the paths of `send-files`.
const PATH_SEPARATOR: char = '|';

/// The stdout line of `event`, without the line break.
pub fn event_line(event: &TransferEvent) -> String {
    let body = match event {
        TransferEvent::Started(info) => {
            let direction = match info.direction {
                Direction::Sending => "s",
                Direction::Receiving => "r",
            };
            let kind = match info.kind {
                Kind::Push => "push",
                Kind::Paste => "paste",
            };
            format!(
                "{STARTED}:{}:{direction}:{kind}:{}:{}:{}",
                info.id.0,
                info.files,
                info.total_bytes,
                single_line(&info.name)
            )
        }
        TransferEvent::Progress { id, done_bytes } => format!("{PROGRESS}:{}:{done_bytes}", id.0),
        TransferEvent::Finished { id, outcome } => {
            let (tag, detail) = outcome_tag(outcome);
            let mut line = format!("{ENDED}:{}:{tag}", id.0);
            if let Some(detail) = detail {
                line.push(SEPARATOR);
                line.push_str(&single_line(&detail));
            }
            line
        }
    };
    format!("{LINE_PREFIX}{body}")
}

/// The event in a viewer's stdout line; `None` for any other line.
pub fn parse_event(line: &str) -> Option<TransferEvent> {
    let rest = line.trim_end().strip_prefix(LINE_PREFIX)?;
    let (kind, rest) = rest.split_once(SEPARATOR)?;
    match kind {
        STARTED => {
            let mut fields = rest.splitn(6, SEPARATOR);
            let id = TransferId(fields.next()?.parse().ok()?);
            let direction = match fields.next()? {
                "s" => Direction::Sending,
                "r" => Direction::Receiving,
                _ => return None,
            };
            let kind = match fields.next()? {
                "push" => Kind::Push,
                "paste" => Kind::Paste,
                _ => return None,
            };
            Some(TransferEvent::Started(TransferInfo {
                id,
                direction,
                kind,
                files: fields.next()?.parse().ok()?,
                total_bytes: fields.next()?.parse().ok()?,
                name: fields.next()?.to_string(),
            }))
        }
        PROGRESS => {
            let (id, done) = rest.split_once(SEPARATOR)?;
            Some(TransferEvent::Progress {
                id: TransferId(id.parse().ok()?),
                done_bytes: done.parse().ok()?,
            })
        }
        ENDED => {
            let (id, rest) = rest.split_once(SEPARATOR)?;
            let (tag, detail) = rest.split_once(SEPARATOR).unwrap_or((rest, ""));
            Some(TransferEvent::Finished {
                id: TransferId(id.parse().ok()?),
                outcome: outcome_of(tag, detail)?,
            })
        }
        _ => None,
    }
}

fn outcome_tag(outcome: &Outcome) -> (&'static str, Option<String>) {
    match outcome {
        Outcome::Completed { folder } => ("done", folder.as_ref().map(|f| f.display().to_string())),
        Outcome::Cancelled { by_peer: false } => ("cancelled", None),
        Outcome::Cancelled { by_peer: true } => ("cancelled-there", None),
        Outcome::Refused(TransferError::NotAllowed) => ("refused-not-allowed", None),
        Outcome::Refused(TransferError::Invalid) => ("refused-invalid", None),
        Outcome::Refused(TransferError::Unavailable) => ("refused-unavailable", None),
        Outcome::Refused(TransferError::Failed(detail)) => ("refused", Some(detail.clone())),
        Outcome::Failed(detail) => ("failed", Some(detail.clone())),
        Outcome::Interrupted => ("interrupted", None),
    }
}

fn outcome_of(tag: &str, detail: &str) -> Option<Outcome> {
    Some(match tag {
        "done" => Outcome::Completed {
            folder: (!detail.is_empty()).then(|| PathBuf::from(detail)),
        },
        "cancelled" => Outcome::Cancelled { by_peer: false },
        "cancelled-there" => Outcome::Cancelled { by_peer: true },
        "refused-not-allowed" => Outcome::Refused(TransferError::NotAllowed),
        "refused-invalid" => Outcome::Refused(TransferError::Invalid),
        "refused-unavailable" => Outcome::Refused(TransferError::Unavailable),
        "refused" => Outcome::Refused(TransferError::Failed(detail.to_string())),
        "failed" => Outcome::Failed(detail.to_string()),
        "interrupted" => Outcome::Interrupted,
        _ => return None,
    })
}

/// The stdin line of `command`, without the line break. Paths holding a
/// line break or `|` cannot be named on Windows; they are left out.
pub fn command_line(command: &TransferCommand) -> String {
    let body = match command {
        TransferCommand::Send(paths) => {
            let paths: Vec<String> = paths
                .iter()
                .map(|p| p.display().to_string())
                .filter(|p| !p.contains(['\r', '\n', PATH_SEPARATOR]))
                .collect();
            format!(
                "{SEND_FILES}{SEPARATOR}{}",
                paths.join(&PATH_SEPARATOR.to_string())
            )
        }
        TransferCommand::Cancel(id) => format!("{CANCEL}{SEPARATOR}{}", id.0),
        TransferCommand::CancelAll => CANCEL_ALL.to_string(),
    };
    format!("{LINE_PREFIX}{body}")
}

/// The command in a line of stdin; `None` for any other line.
pub fn parse_command(line: &str) -> Option<TransferCommand> {
    let rest = line
        .trim_end_matches(['\r', '\n'])
        .strip_prefix(LINE_PREFIX)?;
    if rest == CANCEL_ALL {
        return Some(TransferCommand::CancelAll);
    }
    let (kind, value) = rest.split_once(SEPARATOR)?;
    match kind {
        SEND_FILES => {
            let paths: Vec<PathBuf> = value
                .split(PATH_SEPARATOR)
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect();
            (!paths.is_empty()).then_some(TransferCommand::Send(paths))
        }
        CANCEL => value
            .parse()
            .ok()
            .map(|id| TransferCommand::Cancel(TransferId(id))),
        _ => None,
    }
}

/// Reads the interface's commands from stdin on a thread of its own until
/// stdin ends or the viewer no longer takes commands.
pub fn read_commands(commands: UnboundedSender<TransferCommand>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("wshare-commands".into())
        .spawn(move || forward_commands(std::io::stdin().lock(), &commands))
        .map(|_| ())
}

fn forward_commands(input: impl BufRead, commands: &UnboundedSender<TransferCommand>) {
    for line in input.lines().map_while(Result::ok) {
        if let Some(command) = parse_command(&line)
            && commands.send(command).is_err()
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    fn events() -> Vec<TransferEvent> {
        let id = TransferId(7);
        let mut events = vec![
            TransferEvent::Started(TransferInfo {
                id,
                direction: Direction::Receiving,
                kind: Kind::Push,
                name: "Foto: 2024".into(),
                files: 3,
                total_bytes: 1 << 40,
            }),
            TransferEvent::Started(TransferInfo {
                id,
                direction: Direction::Sending,
                kind: Kind::Paste,
                name: String::new(),
                files: 1,
                total_bytes: 0,
            }),
            TransferEvent::Progress {
                id,
                done_bytes: u64::MAX,
            },
        ];
        for outcome in [
            Outcome::Completed {
                folder: Some(PathBuf::from("C:\\Users\\a\\Downloads\\wshare")),
            },
            Outcome::Completed { folder: None },
            Outcome::Cancelled { by_peer: false },
            Outcome::Cancelled { by_peer: true },
            Outcome::Refused(TransferError::NotAllowed),
            Outcome::Refused(TransferError::Invalid),
            Outcome::Refused(TransferError::Unavailable),
            Outcome::Refused(TransferError::Failed("disco: pieno".into())),
            Outcome::Failed("rete: persa".into()),
            Outcome::Interrupted,
        ] {
            events.push(TransferEvent::Finished { id, outcome });
        }
        events
    }

    #[test]
    fn every_event_survives_its_line() {
        for event in events() {
            let line = event_line(&event);
            assert!(!line.contains(['\r', '\n']));
            assert_eq!(parse_event(&line), Some(event), "{line}");
        }
    }

    #[test]
    fn event_lines_have_a_fixed_shape() {
        let lines: Vec<String> = events().iter().map(event_line).collect();
        assert_eq!(
            lines[0],
            "@wshare:transfer-started:7:r:push:3:1099511627776:Foto: 2024"
        );
        assert_eq!(
            lines[2],
            format!("@wshare:transfer-progress:7:{}", u64::MAX)
        );
        assert_eq!(
            lines[3],
            "@wshare:transfer-ended:7:done:C:\\Users\\a\\Downloads\\wshare"
        );
        assert_eq!(lines[12], "@wshare:transfer-ended:7:interrupted");
    }

    #[test]
    fn a_name_cannot_break_the_line() {
        let event = TransferEvent::Started(TransferInfo {
            id: TransferId(1),
            direction: Direction::Receiving,
            kind: Kind::Push,
            name: "a\r\n@wshare:cancel-transfers".into(),
            files: 1,
            total_bytes: 1,
        });
        let line = event_line(&event);
        assert!(!line.contains('\n'));
        assert!(matches!(
            parse_event(&line),
            Some(TransferEvent::Started(_))
        ));
    }

    #[test]
    fn other_lines_are_not_events() {
        for line in [
            "",
            "Connected to \"UFFICIO\"",
            "@wshare:connected:UFFICIO",
            "@wshare:transfer-started:x:r:push:1:1:a",
            "@wshare:transfer-started:1:q:push:1:1:a",
            "@wshare:transfer-started:1:r:push:1",
            "@wshare:transfer-progress:1",
            "@wshare:transfer-ended:1:exploded",
            "@wshare:transfer-ended:1",
        ] {
            assert_eq!(parse_event(line), None, "{line:?}");
        }
    }

    #[test]
    fn every_command_survives_its_line() {
        for command in [
            TransferCommand::Send(vec![
                PathBuf::from("C:\\Users\\a\\Foto"),
                PathBuf::from("D:\\nota è.txt"),
            ]),
            TransferCommand::Cancel(TransferId(42)),
            TransferCommand::CancelAll,
        ] {
            let line = command_line(&command);
            assert_eq!(
                parse_command(&format!("{line}\r\n")),
                Some(command),
                "{line}"
            );
        }
        assert_eq!(
            command_line(&TransferCommand::CancelAll),
            "@wshare:cancel-transfers"
        );
    }

    #[test]
    fn typing_on_a_terminal_is_never_a_command() {
        for line in [
            "",
            "123456",
            "send-files:C:\\x",
            "@wshare:send-files:",
            "@wshare:cancel-transfer:abc",
            "@wshare:format-c",
            " @wshare:cancel-transfers",
        ] {
            assert_eq!(parse_command(line), None, "{line:?}");
        }
    }

    #[test]
    fn commands_are_read_line_by_line_until_the_input_ends() {
        let input = "PIN typed by mistake\n\
                     @wshare:cancel-transfer:3\n\
                     @wshare:send-files:C:\\a|C:\\b\n";
        let (tx, mut rx) = unbounded_channel();
        forward_commands(input.as_bytes(), &tx);
        assert_eq!(rx.try_recv(), Ok(TransferCommand::Cancel(TransferId(3))));
        assert_eq!(
            rx.try_recv(),
            Ok(TransferCommand::Send(vec![
                PathBuf::from("C:\\a"),
                PathBuf::from("C:\\b")
            ]))
        );
        assert!(rx.try_recv().is_err());
    }
}
