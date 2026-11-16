//! What the settings page shows about unattended access. Pure.

use crate::text;

use super::ports::UnattendedOutcome;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnattendedState {
    /// Whether the service is installed; `None`: Windows could not say.
    pub installed: Option<bool>,
    /// Turning it on (`Some(true)`) or off is under way (Windows may be
    /// asking the user).
    pub pending: Option<bool>,
    /// How the last attempt went, or why the state is unknown.
    pub notice: Option<String>,
}

impl UnattendedState {
    /// The state read from Windows.
    pub fn read(installed: anyhow::Result<bool>) -> Self {
        let mut state = Self::default();
        state.refresh(installed);
        state
    }

    /// While installed, the service shares this PC: the window must not.
    pub fn shares_this_pc(&self) -> bool {
        self.installed == Some(true)
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Turning access on (`enabling`) or off starts.
    pub fn begin(&mut self, enabling: bool) {
        self.pending = Some(enabling);
        self.notice = None;
    }

    /// The attempt is over, and Windows now says `installed`.
    pub fn finished(&mut self, outcome: &UnattendedOutcome, installed: anyhow::Result<bool>) {
        let enabling = self.pending.take().unwrap_or(false);
        self.refresh(installed);
        let result = match outcome {
            UnattendedOutcome::Done if enabling => text::UNATTENDED_ENABLED.to_string(),
            UnattendedOutcome::Done => text::UNATTENDED_DISABLED.to_string(),
            UnattendedOutcome::Cancelled => text::UNATTENDED_CANCELLED.to_string(),
            UnattendedOutcome::Failed(detail) => text::unattended_failed(detail),
        };
        self.notice = Some(match self.notice.take() {
            Some(unknown) => format!("{result} {unknown}"),
            None => result,
        });
    }

    fn refresh(&mut self, installed: anyhow::Result<bool>) {
        match installed {
            Ok(installed) => {
                self.installed = Some(installed);
                self.notice = None;
            }
            Err(e) => {
                self.installed = None;
                self.notice = Some(text::unattended_unknown(&format!("{e:#}")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    #[test]
    fn only_an_installed_service_shares_this_pc() {
        assert!(UnattendedState::read(Ok(true)).shares_this_pc());
        assert!(!UnattendedState::read(Ok(false)).shares_this_pc());
        let unknown = UnattendedState::read(Err(anyhow!("no access")));
        assert!(!unknown.shares_this_pc());
        assert_eq!(unknown.installed, None);
        assert!(unknown.notice.unwrap().contains("no access"));
    }

    #[test]
    fn an_attempt_says_how_it_went() {
        let mut state = UnattendedState::read(Ok(false));
        state.begin(true);
        assert!(state.busy());
        state.finished(&UnattendedOutcome::Done, Ok(true));
        assert!(!state.busy());
        assert!(state.shares_this_pc());
        assert_eq!(state.notice.as_deref(), Some(text::UNATTENDED_ENABLED));

        state.begin(false);
        assert_eq!(state.notice, None);
        state.finished(&UnattendedOutcome::Done, Ok(false));
        assert_eq!(state.notice.as_deref(), Some(text::UNATTENDED_DISABLED));
    }

    #[test]
    fn a_cancelled_or_failed_attempt_keeps_what_windows_says() {
        let mut state = UnattendedState::read(Ok(false));
        state.begin(true);
        state.finished(&UnattendedOutcome::Cancelled, Ok(false));
        assert!(!state.shares_this_pc());
        assert_eq!(state.notice.as_deref(), Some(text::UNATTENDED_CANCELLED));

        state.begin(true);
        let failed = UnattendedOutcome::Failed("codice 1".into());
        state.finished(&failed, Err(anyhow!("no access")));
        assert_eq!(state.installed, None);
        let notice = state.notice.unwrap();
        assert!(
            notice.contains("codice 1") && notice.contains("no access"),
            "{notice}"
        );
    }
}
