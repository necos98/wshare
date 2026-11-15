//! What the viewer window tells the user over the picture: the shortcuts
//! when the session starts, a change of the host's permission for remote
//! control, and the warning that the picture stopped. Pure: the render loop
//! feeds it the time and draws what it says.

use std::time::{Duration, Instant};

use crate::text;

/// How long the shortcuts stay visible after the session starts.
pub const SHORTCUTS_FOR: Duration = Duration::from_secs(4);
/// How long a change of the host's permission stays visible.
pub const CONTROL_CHANGE_FOR: Duration = Duration::from_secs(3);

/// A message that goes away by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    Shortcuts,
    /// The host switched remote control off.
    ControlOff,
    /// The host switched remote control on again.
    ControlOn,
}

impl Notice {
    fn lines(self) -> Vec<String> {
        match self {
            Self::Shortcuts => text::NOTICE_SHORTCUTS.map(String::from).to_vec(),
            Self::ControlOff => vec![text::NOTICE_CONTROL_OFF.to_string()],
            Self::ControlOn => vec![text::NOTICE_CONTROL_ON.to_string()],
        }
    }

    fn lasts(self) -> Duration {
        match self {
            Self::Shortcuts => SHORTCUTS_FOR,
            Self::ControlOff | Self::ControlOn => CONTROL_CHANGE_FOR,
        }
    }
}

/// The lines to show now: the warning that the picture stopped while it
/// lasts, else the latest timed notice until it expires.
#[derive(Debug, Default)]
pub struct NoticeBoard {
    timed: Option<(Notice, Instant)>,
    unstable: bool,
    /// What the renderer was last told (`None` before the first time).
    sent: Option<Option<Vec<String>>>,
}

impl NoticeBoard {
    /// Shows `notice` for its time, replacing the one on screen.
    pub fn show(&mut self, notice: Notice, now: Instant) {
        self.timed = Some((notice, now + notice.lasts()));
    }

    /// The picture stopped (`true`) or came back (`false`).
    pub fn set_unstable(&mut self, unstable: bool) {
        self.unstable = unstable;
    }

    /// The lines to draw if they differ from the last ones returned here
    /// (outer `Some`); inner `None` means nothing to draw any more.
    pub fn take_changed(&mut self, now: Instant) -> Option<Option<Vec<String>>> {
        if self.timed.is_some_and(|(_, until)| now >= until) {
            self.timed = None;
        }
        let lines = self.lines();
        // Nothing was ever drawn: nothing to take away either.
        let unchanged = match &self.sent {
            Some(sent) => *sent == lines,
            None => lines.is_none(),
        };
        if unchanged {
            return None;
        }
        self.sent = Some(lines.clone());
        Some(lines)
    }

    fn lines(&self) -> Option<Vec<String>> {
        if self.unstable {
            return Some(vec![text::NOTICE_UNSTABLE.to_string()]);
        }
        self.timed.map(|(notice, _)| notice.lines())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn one(line: &str) -> Option<Option<Vec<String>>> {
        Some(Some(vec![line.to_string()]))
    }

    #[test]
    fn an_empty_board_draws_nothing_and_says_so_once_at_most() {
        let base = Instant::now();
        let mut board = NoticeBoard::default();
        assert_eq!(board.take_changed(base), None);
    }

    #[test]
    fn the_shortcuts_show_for_four_seconds() {
        let base = Instant::now();
        let mut board = NoticeBoard::default();
        board.show(Notice::Shortcuts, base);
        let shown = board.take_changed(base).unwrap().unwrap();
        assert_eq!(
            shown,
            ["Ctrl+Alt+Invio: schermo intero", "Ctrl+Alt+I: statistiche"]
        );
        assert_eq!(board.take_changed(at(base, 3999)), None, "still the same");
        assert_eq!(board.take_changed(at(base, 4000)), Some(None));
        assert_eq!(board.take_changed(at(base, 5000)), None, "told only once");
    }

    #[test]
    fn a_change_of_permission_replaces_the_shortcuts() {
        let base = Instant::now();
        let mut board = NoticeBoard::default();
        board.show(Notice::Shortcuts, base);
        board.take_changed(base);
        board.show(Notice::ControlOff, at(base, 1000));
        assert_eq!(
            board.take_changed(at(base, 1000)),
            one("Controllo remoto disattivato dall'altro computer")
        );
        assert_eq!(board.take_changed(at(base, 3999)), None);
        assert_eq!(board.take_changed(at(base, 4000)), Some(None));

        board.show(Notice::ControlOn, at(base, 5000));
        assert_eq!(
            board.take_changed(at(base, 5000)),
            one("Controllo remoto riattivato dall'altro computer")
        );
    }

    #[test]
    fn the_warning_wins_while_the_picture_is_stopped_and_goes_with_it() {
        let base = Instant::now();
        let mut board = NoticeBoard::default();
        board.show(Notice::Shortcuts, base);
        board.take_changed(base);

        board.set_unstable(true);
        assert_eq!(
            board.take_changed(at(base, 100)),
            one("Connessione instabile…")
        );
        assert_eq!(board.take_changed(at(base, 200)), None);

        board.set_unstable(false);
        assert_eq!(
            board.take_changed(at(base, 300)).unwrap().unwrap().len(),
            2,
            "the shortcuts are still due"
        );
        assert_eq!(board.take_changed(at(base, 4000)), Some(None));
    }

    #[test]
    fn a_warning_after_the_notices_ended_goes_away_alone() {
        let base = Instant::now();
        let mut board = NoticeBoard::default();
        board.set_unstable(true);
        assert_eq!(board.take_changed(base), one("Connessione instabile…"));
        board.set_unstable(false);
        assert_eq!(board.take_changed(at(base, 1)), Some(None));
        assert_eq!(board.take_changed(at(base, 2)), None);
    }
}
