//! What the settings page shows about unattended access. Pure.

use crate::text;
use crate::unattended::ServiceSettings;

use super::ports::UnattendedOutcome;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnattendedState {
    /// Whether the service is installed; `None`: Windows could not say.
    pub installed: Option<bool>,
    /// Turning it on (or updating it) or off is under way (Windows may be
    /// asking the user).
    pending: Option<Pending>,
    /// How the last attempt went, or why the state is unknown.
    pub notice: Option<String>,
    /// The host settings the service has, as far as this window knows:
    /// those of its last update from here, or the window's when it started
    /// (the service cannot be read without administrator rights).
    applied: Option<ServiceSettings>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    Install(ServiceSettings),
    Uninstall,
}

impl UnattendedState {
    /// The state read from Windows; `current`: the window's settings.
    pub fn read(installed: anyhow::Result<bool>, current: &ServiceSettings) -> Self {
        let mut state = Self::default();
        state.refresh(installed);
        if state.shares_this_pc() {
            state.applied = Some(current.clone());
        }
        state
    }

    /// While installed, the service shares this PC: the window must not.
    pub fn shares_this_pc(&self) -> bool {
        self.installed == Some(true)
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Whether the settings only the window's own sharing uses (remote
    /// control, access mode, received files) can be changed: not while the
    /// service shares this PC, since it ignores them.
    pub fn window_only_settings_editable(&self) -> bool {
        !self.shares_this_pc()
    }

    /// Whether the service has other host settings than `current`, the
    /// window's: they apply only once «Aggiorna» copies them.
    pub fn needs_update(&self, current: &ServiceSettings) -> bool {
        self.shares_this_pc()
            && !self.busy()
            && self
                .applied
                .as_ref()
                .is_some_and(|applied| applied != current)
    }

    /// Turning access on, or updating it, with `settings` starts.
    pub fn begin_install(&mut self, settings: ServiceSettings) {
        self.begin(Pending::Install(settings));
    }

    /// Turning access off starts.
    pub fn begin_uninstall(&mut self) {
        self.begin(Pending::Uninstall);
    }

    fn begin(&mut self, pending: Pending) {
        self.pending = Some(pending);
        self.notice = None;
    }

    /// The attempt is over, and Windows now says `installed`.
    pub fn finished(&mut self, outcome: &UnattendedOutcome, installed: anyhow::Result<bool>) {
        let pending = self.pending.take();
        let enabling = matches!(pending, Some(Pending::Install(_)));
        if let (UnattendedOutcome::Done, Some(Pending::Install(settings))) = (outcome, pending) {
            self.applied = Some(settings);
        }
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
                if !installed {
                    self.applied = None;
                }
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

    fn read(installed: anyhow::Result<bool>) -> UnattendedState {
        UnattendedState::read(installed, &ServiceSettings::default())
    }

    fn private() -> ServiceSettings {
        ServiceSettings {
            share_clipboard: false,
            ..ServiceSettings::default()
        }
    }

    #[test]
    fn only_an_installed_service_shares_this_pc() {
        assert!(read(Ok(true)).shares_this_pc());
        assert!(!read(Ok(false)).shares_this_pc());
        let unknown = read(Err(anyhow!("no access")));
        assert!(!unknown.shares_this_pc());
        assert_eq!(unknown.installed, None);
        assert!(unknown.notice.unwrap().contains("no access"));
    }

    #[test]
    fn with_the_service_installed_the_settings_it_ignores_are_disabled() {
        assert!(!read(Ok(true)).window_only_settings_editable());
        assert!(read(Ok(false)).window_only_settings_editable());
        assert!(
            read(Err(anyhow!("no access"))).window_only_settings_editable(),
            "the window may still be the one sharing"
        );
    }

    #[test]
    fn a_change_to_the_service_settings_asks_for_an_update() {
        let defaults = ServiceSettings::default();
        let mut state = read(Ok(true));
        assert!(!state.needs_update(&defaults), "as the window started");
        assert!(state.needs_update(&private()));

        state.begin_install(private());
        assert!(!state.needs_update(&private()), "not while it is copied");
        state.finished(&UnattendedOutcome::Done, Ok(true));
        assert!(!state.needs_update(&private()));
        assert!(state.needs_update(&defaults));
    }

    #[test]
    fn a_failed_update_still_asks_for_one() {
        let mut state = read(Ok(true));
        state.begin_install(private());
        state.finished(&UnattendedOutcome::Cancelled, Ok(true));
        assert!(state.needs_update(&private()));
    }

    #[test]
    fn without_the_service_nothing_needs_an_update() {
        assert!(!read(Ok(false)).needs_update(&private()));
        let mut state = read(Ok(true));
        state.begin_uninstall();
        state.finished(&UnattendedOutcome::Done, Ok(false));
        assert!(!state.needs_update(&private()));
    }

    #[test]
    fn an_attempt_says_how_it_went() {
        let mut state = read(Ok(false));
        state.begin_install(ServiceSettings::default());
        assert!(state.busy());
        state.finished(&UnattendedOutcome::Done, Ok(true));
        assert!(!state.busy());
        assert!(state.shares_this_pc());
        assert_eq!(state.notice.as_deref(), Some(text::UNATTENDED_ENABLED));

        state.begin_uninstall();
        assert_eq!(state.notice, None);
        state.finished(&UnattendedOutcome::Done, Ok(false));
        assert_eq!(state.notice.as_deref(), Some(text::UNATTENDED_DISABLED));
    }

    #[test]
    fn a_cancelled_or_failed_attempt_keeps_what_windows_says() {
        let mut state = read(Ok(false));
        state.begin_install(ServiceSettings::default());
        state.finished(&UnattendedOutcome::Cancelled, Ok(false));
        assert!(!state.shares_this_pc());
        assert_eq!(state.notice.as_deref(), Some(text::UNATTENDED_CANCELLED));

        state.begin_install(ServiceSettings::default());
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
