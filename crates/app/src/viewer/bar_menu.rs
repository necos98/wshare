//! The control bar's drop-down menus: what they offer (the host's monitors,
//! the quality levels, what can be done with files) with the choice in
//! effect, and the state of the one open. Pure, like the bar itself.

use protocol::session::{MAX_MONITORS, MonitorEntry, Quality, bounded_name};
use render::MenuView;

use super::toolbar::BarAction;
use crate::text;

/// Which menu a button opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    /// The host's monitors.
    Screen,
    Quality,
    /// Sending files, cancelling transfers, the folder of received files.
    Files,
}

/// A row of the files menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileRow {
    Send,
    Cancel,
    OpenReceived,
}

impl FileRow {
    fn action(self) -> BarAction {
        match self {
            Self::Send => BarAction::SendFiles,
            Self::Cancel => BarAction::CancelTransfers,
            Self::OpenReceived => BarAction::OpenReceived,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Send => text::FILES_SEND,
            Self::Cancel => text::FILES_CANCEL,
            Self::OpenReceived => text::FILES_OPEN_RECEIVED,
        }
    }
}

/// What the files menu can offer now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileState {
    /// Transfers are in progress: they can be cancelled.
    pub transfers: bool,
    /// Files were received: their folder can be opened.
    pub received: bool,
}

/// What the menus offer, as the host last told and the user last chose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionChoices {
    monitors: Vec<MonitorEntry>,
    /// Position in `monitors` of the one shared, if within the list.
    current_monitor: Option<usize>,
    quality: Quality,
    files: FileState,
}

impl SessionChoices {
    /// The host's monitors and the one it shares (an index into them). At
    /// most [`MAX_MONITORS`] are kept, with names of at most
    /// [`protocol::session::MAX_MONITOR_NAME`] characters, whatever the host
    /// sent.
    pub fn set_monitors(&mut self, mut monitors: Vec<MonitorEntry>, current: u32) {
        monitors.truncate(MAX_MONITORS);
        for monitor in &mut monitors {
            monitor.name = bounded_name(&monitor.name);
        }
        self.current_monitor = usize::try_from(current)
            .ok()
            .filter(|current| *current < monitors.len());
        self.monitors = monitors;
    }

    /// The quality the user picked (the host does not confirm it).
    pub fn set_quality(&mut self, quality: Quality) {
        self.quality = quality;
    }

    /// What the files menu offers from now on.
    pub fn set_files(&mut self, files: FileState) {
        self.files = files;
    }

    fn file_rows(&self) -> Vec<FileRow> {
        let mut rows = vec![FileRow::Send];
        if self.files.transfers {
            rows.push(FileRow::Cancel);
        }
        if self.files.received {
            rows.push(FileRow::OpenReceived);
        }
        rows
    }

    /// The rows of `kind`, top to bottom.
    pub fn labels(&self, kind: MenuKind) -> Vec<String> {
        match kind {
            MenuKind::Files => self
                .file_rows()
                .into_iter()
                .map(|row| row.label().to_string())
                .collect(),
            MenuKind::Screen => self
                .monitors
                .iter()
                .enumerate()
                .map(|(position, monitor)| text::monitor_entry_label(position, monitor))
                .collect(),
            MenuKind::Quality => Quality::ALL
                .into_iter()
                .map(|quality| text::quality_label(quality).to_string())
                .collect(),
        }
    }

    pub fn len(&self, kind: MenuKind) -> usize {
        match kind {
            MenuKind::Screen => self.monitors.len(),
            MenuKind::Quality => Quality::ALL.len(),
            MenuKind::Files => self.file_rows().len(),
        }
    }

    /// The row of `kind` in effect now.
    pub fn checked(&self, kind: MenuKind) -> Option<usize> {
        match kind {
            MenuKind::Screen => self.current_monitor,
            MenuKind::Quality => Quality::ALL.iter().position(|q| *q == self.quality),
            MenuKind::Files => None,
        }
    }

    /// What choosing `row` of `kind` does; `None` past the last row.
    pub fn action(&self, kind: MenuKind, row: usize) -> Option<BarAction> {
        if row >= self.len(kind) {
            return None;
        }
        Some(match kind {
            MenuKind::Screen => BarAction::SelectMonitor(u32::try_from(row).ok()?),
            MenuKind::Quality => BarAction::SetQuality(Quality::ALL[row]),
            MenuKind::Files => self.file_rows()[row].action(),
        })
    }
}

/// The menu open under a button of the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenMenu {
    pub kind: MenuKind,
    /// The bar button it hangs under.
    pub button: usize,
    /// Row under the pointer.
    pub hovered: Option<usize>,
    /// Row the primary button went down on.
    pub pressed: Option<usize>,
}

impl OpenMenu {
    pub fn new(kind: MenuKind, button: usize) -> Self {
        Self {
            kind,
            button,
            hovered: None,
            pressed: None,
        }
    }

    /// What the renderer draws for it.
    pub fn view(&self, choices: &SessionChoices) -> MenuView {
        MenuView {
            button: self.button,
            items: choices.labels(self.kind),
            checked: choices.checked(self.kind),
            hovered: self.hovered,
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::session::MAX_MONITOR_NAME;

    use super::*;

    fn two_monitors() -> Vec<MonitorEntry> {
        vec![
            MonitorEntry::new("DISPLAY1", 1920, 1080, true),
            MonitorEntry::new("DISPLAY2", 2560, 1440, false),
        ]
    }

    #[test]
    fn the_screen_menu_lists_the_host_monitors_and_checks_the_shared_one() {
        let mut choices = SessionChoices::default();
        assert_eq!(choices.len(MenuKind::Screen), 0, "known after Welcome");
        choices.set_monitors(two_monitors(), 1);
        assert_eq!(
            choices.labels(MenuKind::Screen),
            [
                "1. DISPLAY1 (1920×1080, principale)",
                "2. DISPLAY2 (2560×1440)"
            ]
        );
        assert_eq!(choices.checked(MenuKind::Screen), Some(1));
        assert_eq!(
            choices.action(MenuKind::Screen, 0),
            Some(BarAction::SelectMonitor(0))
        );
        assert_eq!(choices.action(MenuKind::Screen, 2), None);
    }

    #[test]
    fn a_current_monitor_outside_the_list_checks_nothing() {
        let mut choices = SessionChoices::default();
        choices.set_monitors(two_monitors(), 7);
        assert_eq!(choices.checked(MenuKind::Screen), None);
    }

    #[test]
    fn an_oversized_list_is_cut() {
        let mut choices = SessionChoices::default();
        let many = vec![MonitorEntry::new("M", 1, 1, false); MAX_MONITORS + 3];
        choices.set_monitors(many, 0);
        assert_eq!(choices.len(MenuKind::Screen), MAX_MONITORS);
        let long = MonitorEntry {
            name: "x".repeat(MAX_MONITOR_NAME * 4),
            width: 1,
            height: 1,
            primary: false,
        };
        choices.set_monitors(vec![long], 0);
        let label = &choices.labels(MenuKind::Screen)[0];
        assert!(label.chars().count() < MAX_MONITOR_NAME + 20, "{label}");
    }

    #[test]
    fn the_quality_menu_starts_on_automatic_and_follows_the_choice() {
        let mut choices = SessionChoices::default();
        assert_eq!(
            choices.labels(MenuKind::Quality),
            ["Automatica", "Alta", "Media", "Bassa"]
        );
        assert_eq!(choices.checked(MenuKind::Quality), Some(0));
        assert_eq!(
            choices.action(MenuKind::Quality, 3),
            Some(BarAction::SetQuality(Quality::Low))
        );
        choices.set_quality(Quality::Medium);
        assert_eq!(choices.checked(MenuKind::Quality), Some(2));
        assert_eq!(choices.action(MenuKind::Quality, 4), None);
    }

    #[test]
    fn the_files_menu_offers_what_can_be_done_now() {
        let mut choices = SessionChoices::default();
        assert_eq!(choices.labels(MenuKind::Files), ["Invia file…"]);
        assert_eq!(
            choices.action(MenuKind::Files, 0),
            Some(BarAction::SendFiles)
        );
        assert_eq!(choices.checked(MenuKind::Files), None);
        choices.set_files(FileState {
            transfers: true,
            received: true,
        });
        assert_eq!(choices.len(MenuKind::Files), 3);
        assert_eq!(
            choices.action(MenuKind::Files, 1),
            Some(BarAction::CancelTransfers)
        );
        assert_eq!(
            choices.action(MenuKind::Files, 2),
            Some(BarAction::OpenReceived)
        );
        choices.set_files(FileState {
            transfers: false,
            received: true,
        });
        assert_eq!(
            choices.action(MenuKind::Files, 1),
            Some(BarAction::OpenReceived)
        );
        assert_eq!(choices.action(MenuKind::Files, 2), None);
    }

    #[test]
    fn an_open_menu_shows_its_rows_check_and_hover() {
        let mut choices = SessionChoices::default();
        choices.set_monitors(two_monitors(), 0);
        let mut menu = OpenMenu::new(MenuKind::Screen, 1);
        menu.hovered = Some(1);
        let view = menu.view(&choices);
        assert_eq!(view.button, 1);
        assert_eq!(view.items.len(), 2);
        assert_eq!((view.checked, view.hovered), (Some(0), Some(1)));
    }
}
