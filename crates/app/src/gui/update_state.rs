//! What the settings page shows about updates. Pure.

use crate::text;
use crate::update::{Available, UpdateFailure};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateState {
    pub phase: UpdatePhase,
    /// Why the last check or installation failed.
    pub notice: Option<String>,
    /// The unattended-access service runs another wshare than this window
    /// (see `Updates::service_outdated`).
    pub service_outdated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UpdatePhase {
    /// Nothing checked yet (or the check failed).
    #[default]
    Unknown,
    Checking,
    UpToDate,
    Available(Available),
    /// Downloading and putting the release in place.
    Installing(Available),
    /// Installed, but wshare could not start again by itself.
    RestartByHand(String),
}

impl UpdateState {
    pub fn new(service_outdated: bool) -> Self {
        Self {
            service_outdated,
            ..Self::default()
        }
    }

    /// A check or an installation is under way.
    pub fn busy(&self) -> bool {
        matches!(
            self.phase,
            UpdatePhase::Checking | UpdatePhase::Installing(_)
        )
    }

    /// The release that can be installed now, if any.
    pub fn available(&self) -> Option<&Available> {
        match &self.phase {
            UpdatePhase::Available(available) => Some(available),
            _ => None,
        }
    }

    pub fn begin_check(&mut self) {
        self.phase = UpdatePhase::Checking;
        self.notice = None;
    }

    pub fn checked(&mut self, result: Result<Option<Available>, UpdateFailure>) {
        self.phase = match result {
            Ok(Some(available)) => UpdatePhase::Available(available),
            Ok(None) => UpdatePhase::UpToDate,
            Err(failure) => {
                self.notice = Some(text::update_failure(&failure));
                UpdatePhase::Unknown
            }
        };
    }

    /// Starts installing the release found; `None` if there is none.
    pub fn begin_install(&mut self) -> Option<Available> {
        let available = self.available()?.clone();
        self.phase = UpdatePhase::Installing(available.clone());
        self.notice = None;
        Some(available)
    }

    /// The installation failed: the release can be tried again.
    pub fn install_failed(&mut self, failure: &UpdateFailure) {
        if let UpdatePhase::Installing(available) = &self.phase {
            self.phase = UpdatePhase::Available(available.clone());
        }
        self.notice = Some(text::update_failure(failure));
    }

    /// Installed, but starting the new program failed.
    pub fn restart_failed(&mut self, detail: &str) {
        let version = match &self.phase {
            UpdatePhase::Installing(available) => available.version().to_string(),
            _ => String::new(),
        };
        self.phase = UpdatePhase::RestartByHand(version);
        self.notice = Some(text::update_restart_failed(detail));
    }
}
