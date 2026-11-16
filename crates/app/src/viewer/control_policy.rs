//! Who may enable remote control: both the host and the viewer's user.

use super::notice::Notice;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlPolicy {
    pub host_allows: bool,
    /// The viewer chose `--view-only`.
    pub view_only: bool,
}

impl ControlPolicy {
    pub fn enabled(self) -> bool {
        self.host_allows && !self.view_only
    }

    /// Records that the host now allows (or forbids) remote control.
    /// Returns what to tell the user when this changed whether they can
    /// control the host.
    pub fn set_host_allows(&mut self, allowed: bool) -> Option<Notice> {
        let was_enabled = self.enabled();
        self.host_allows = allowed;
        match (was_enabled, self.enabled()) {
            (true, false) => Some(Notice::ControlOff),
            (false, true) => Some(Notice::ControlOn),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_needs_host_permission_and_viewer_consent() {
        let policy = |host_allows, view_only| ControlPolicy {
            host_allows,
            view_only,
        };
        assert!(policy(true, false).enabled());
        assert!(!policy(false, false).enabled());
        assert!(!policy(true, true).enabled());
    }

    #[test]
    fn the_user_hears_when_the_host_switches_control_off_and_on() {
        let mut policy = ControlPolicy {
            host_allows: true,
            view_only: false,
        };
        assert_eq!(policy.set_host_allows(true), None, "nothing changed");
        assert_eq!(policy.set_host_allows(false), Some(Notice::ControlOff));
        assert!(!policy.enabled());
        assert_eq!(policy.set_host_allows(false), None, "already off");
        assert_eq!(policy.set_host_allows(true), Some(Notice::ControlOn));
        assert!(policy.enabled());
    }

    #[test]
    fn a_view_only_user_has_nothing_to_hear_about() {
        let mut policy = ControlPolicy {
            host_allows: true,
            view_only: true,
        };
        assert_eq!(policy.set_host_allows(false), None);
        assert_eq!(policy.set_host_allows(true), None);
        assert!(!policy.enabled());
    }
}
