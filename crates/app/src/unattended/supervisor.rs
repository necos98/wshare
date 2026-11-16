//! When the service starts, stops and starts again its agent. Pure policy,
//! unit-tested; `service_main` acts on its decisions.
//!
//! There is one agent, in the session attached to the console (the screen
//! and keyboard of the PC): when that session changes (sign-in, sign-out,
//! user switch) the agent of the old one stops and a new one starts. An
//! agent that ends soon after starting (e.g. its port is taken) is started
//! again only after a pause, so a lasting problem cannot spin.

use std::time::{Duration, Instant};

/// An agent that lived less than this ended too soon.
pub(super) const MIN_UPTIME: Duration = Duration::from_secs(10);
/// The pause before starting again an agent that ended too soon.
pub(super) const RETRY_DELAY: Duration = Duration::from_secs(5);

/// The agent the service runs, as seen now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AgentStatus {
    pub(super) session: u32,
    pub(super) started: Instant,
    pub(super) running: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Decision {
    /// Nothing to do.
    Keep,
    /// Start an agent in this session.
    Launch(u32),
    /// Stop the agent: its session is no longer at the console.
    Stop,
    /// The agent ended on its own: let go of it.
    Reap,
}

#[derive(Debug, Default)]
pub(super) struct Supervisor {
    /// No agent starts in this session before this time.
    pause: Option<(u32, Instant)>,
}

impl Supervisor {
    /// What to do, given the session at the console (`None`: none, e.g.
    /// while it switches) and the agent running, if any.
    pub(super) fn decide(
        &mut self,
        console: Option<u32>,
        agent: Option<AgentStatus>,
        now: Instant,
    ) -> Decision {
        match agent {
            Some(agent) if !agent.running => {
                if now.duration_since(agent.started) < MIN_UPTIME {
                    self.pause = Some((agent.session, now + RETRY_DELAY));
                }
                Decision::Reap
            }
            Some(agent) if Some(agent.session) == console => Decision::Keep,
            Some(_) => Decision::Stop,
            None => match console {
                Some(session) if self.paused(session, now) => Decision::Keep,
                Some(session) => {
                    self.pause = None;
                    Decision::Launch(session)
                }
                None => Decision::Keep,
            },
        }
    }

    /// The agent of `session` could not even start: it is tried again
    /// after the pause, as one that ended too soon.
    pub(super) fn launch_failed(&mut self, session: u32, now: Instant) {
        self.pause = Some((session, now + RETRY_DELAY));
    }

    /// A new session is not the old problem: only the session whose agent
    /// ended too soon waits.
    fn paused(&self, session: u32, now: Instant) -> bool {
        self.pause
            .is_some_and(|(paused, until)| paused == session && now < until)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(session: u32, started: Instant, running: bool) -> Option<AgentStatus> {
        Some(AgentStatus {
            session,
            started,
            running,
        })
    }

    #[test]
    fn an_agent_starts_in_the_session_at_the_console() {
        let mut supervisor = Supervisor::default();
        let now = Instant::now();
        assert_eq!(supervisor.decide(Some(1), None, now), Decision::Launch(1));
        assert_eq!(
            supervisor.decide(None, None, now),
            Decision::Keep,
            "no session at the console: nothing to start"
        );
    }

    #[test]
    fn a_running_agent_of_the_console_session_is_kept() {
        let mut supervisor = Supervisor::default();
        let now = Instant::now();
        assert_eq!(
            supervisor.decide(Some(1), agent(1, now, true), now + MIN_UPTIME),
            Decision::Keep
        );
    }

    #[test]
    fn a_session_change_stops_the_agent_and_starts_one_in_the_new_session() {
        let mut supervisor = Supervisor::default();
        let now = Instant::now();
        assert_eq!(
            supervisor.decide(Some(2), agent(1, now, true), now),
            Decision::Stop
        );
        assert_eq!(supervisor.decide(Some(2), None, now), Decision::Launch(2));
        assert_eq!(
            supervisor.decide(None, agent(2, now, true), now),
            Decision::Stop,
            "the console left the session"
        );
    }

    #[test]
    fn an_agent_that_ended_after_a_while_starts_again_at_once() {
        let mut supervisor = Supervisor::default();
        let started = Instant::now();
        let later = started + MIN_UPTIME;
        assert_eq!(
            supervisor.decide(Some(1), agent(1, started, false), later),
            Decision::Reap
        );
        assert_eq!(supervisor.decide(Some(1), None, later), Decision::Launch(1));
    }

    #[test]
    fn an_agent_that_ended_too_soon_starts_again_after_a_pause() {
        let mut supervisor = Supervisor::default();
        let started = Instant::now();
        let soon = started + Duration::from_secs(1);
        assert_eq!(
            supervisor.decide(Some(1), agent(1, started, false), soon),
            Decision::Reap
        );
        assert_eq!(supervisor.decide(Some(1), None, soon), Decision::Keep);
        assert_eq!(
            supervisor.decide(Some(1), None, soon + RETRY_DELAY),
            Decision::Launch(1)
        );
    }

    #[test]
    fn an_agent_that_could_not_start_is_tried_again_after_a_pause() {
        let mut supervisor = Supervisor::default();
        let now = Instant::now();
        supervisor.launch_failed(1, now);
        assert_eq!(supervisor.decide(Some(1), None, now), Decision::Keep);
        assert_eq!(
            supervisor.decide(Some(1), None, now + RETRY_DELAY),
            Decision::Launch(1)
        );
    }

    #[test]
    fn a_new_session_is_not_paused_for_the_old_problem() {
        let mut supervisor = Supervisor::default();
        let started = Instant::now();
        supervisor.decide(Some(1), agent(1, started, false), started);
        assert_eq!(supervisor.decide(Some(1), None, started), Decision::Keep);
        // Another user signs in meanwhile: their session gets an agent at
        // once.
        assert_eq!(
            supervisor.decide(Some(2), None, started),
            Decision::Launch(2)
        );
    }
}
