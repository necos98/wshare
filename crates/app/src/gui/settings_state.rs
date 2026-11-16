//! What the state says about the settings and the connection indicator.

use super::model::GuiState;
use crate::text;

/// The two pages of the window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Main,
    Settings,
}

impl GuiState {
    /// The sharing runs with streaming settings (or a download folder)
    /// other than the saved ones: they apply after a restart.
    pub fn restart_needed(&self) -> bool {
        self.sharing.enabled
            && self.applied.as_ref().is_some_and(|applied| {
                let saved = self.settings.host_options();
                applied.stream != saved.stream
                    || applied.port != saved.port
                    || applied.downloads != saved.downloads
            })
    }

    /// The line of the always-visible indicator; `None` hides it. It shows
    /// while a viewer is connected, unless the user turned it off.
    pub fn indicator_text(&self) -> Option<String> {
        let viewer = self.sharing.viewer.as_ref()?;
        self.settings
            .connection_indicator
            .then(|| text::indicator_connected(&viewer.name))
    }
}

#[cfg(test)]
mod tests {
    use crate::host::HostEvent;

    use super::super::settings::Settings;
    use super::*;

    fn connected_state() -> GuiState {
        let mut state = GuiState::default();
        state.sharing.started();
        state.apply_host_event(HostEvent::ViewerConnected {
            viewer_name: "PORTATILE".into(),
            address: "192.168.1.7:5".parse().unwrap(),
        });
        state
    }

    #[test]
    fn the_indicator_follows_the_connection_and_the_setting() {
        let state = GuiState::default();
        assert_eq!(state.indicator_text(), None);

        let mut state = connected_state();
        assert_eq!(
            state.indicator_text().as_deref(),
            Some("PORTATILE è connesso")
        );

        state.settings.connection_indicator = false;
        assert_eq!(state.indicator_text(), None);

        state.settings.connection_indicator = true;
        state.apply_host_event(HostEvent::ViewerDisconnected {
            address: "192.168.1.7:5".parse().unwrap(),
            stats: Default::default(),
            error: None,
        });
        assert_eq!(state.indicator_text(), None);
    }

    #[test]
    fn a_restart_is_needed_only_when_streaming_differs_from_the_running_one() {
        let mut state = GuiState::default();
        assert!(!state.restart_needed());

        state.sharing.started();
        state.applied = Some(state.settings.host_options());
        assert!(!state.restart_needed());

        state.settings.fps = 30;
        assert!(state.restart_needed());
        state.settings = Settings::default();
        assert!(!state.restart_needed());

        state.settings.port = 9000;
        assert!(state.restart_needed());
        state.settings = Settings::default();

        // Switching remote control is live: no restart.
        state.settings.allow_input = false;
        assert!(!state.restart_needed());

        // Nothing runs: nothing to restart.
        state.settings.fps = 30;
        state.sharing.stopped(None);
        assert!(!state.restart_needed());
    }
}
