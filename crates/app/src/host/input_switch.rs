//! What a session does when the host flips the "allow input" switch while a
//! viewer is connected. Pure: the control loop carries out the result.

use protocol::control::ControlMessage;

/// The input permission the viewer was last told (Welcome or
/// `InputAllowed`).
pub(super) struct InputSwitch {
    told: bool,
}

/// Reaction to a (possible) change of the permission.
#[derive(Debug, Default, PartialEq)]
pub(super) struct SwitchActions {
    /// Let go of every key and button the viewer holds.
    pub(super) release: bool,
    /// Message to send to the viewer.
    pub(super) notify: Option<ControlMessage>,
}

impl InputSwitch {
    /// `told`: the value sent in Welcome.
    pub(super) fn new(told: bool) -> Self {
        Self { told }
    }

    /// The permission is now `allowed`. Changes can be coalesced (off and on
    /// again before the loop looks), so only a value different from what
    /// the viewer knows counts.
    pub(super) fn update(&mut self, allowed: bool) -> SwitchActions {
        if allowed == self.told {
            return SwitchActions::default();
        }
        self.told = allowed;
        SwitchActions {
            release: !allowed,
            notify: Some(ControlMessage::InputAllowed(allowed)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidding_releases_and_tells_the_viewer() {
        let mut switch = InputSwitch::new(true);
        assert_eq!(
            switch.update(false),
            SwitchActions {
                release: true,
                notify: Some(ControlMessage::InputAllowed(false)),
            }
        );
    }

    #[test]
    fn allowing_tells_the_viewer_without_releasing() {
        let mut switch = InputSwitch::new(false);
        assert_eq!(
            switch.update(true),
            SwitchActions {
                release: false,
                notify: Some(ControlMessage::InputAllowed(true)),
            }
        );
    }

    #[test]
    fn unchanged_or_coalesced_values_send_nothing() {
        let mut switch = InputSwitch::new(true);
        assert_eq!(switch.update(true), SwitchActions::default());
        switch.update(false);
        assert_eq!(
            switch.update(false),
            SwitchActions::default(),
            "already told"
        );
        assert_eq!(
            switch.update(true).notify,
            Some(ControlMessage::InputAllowed(true))
        );
    }
}
