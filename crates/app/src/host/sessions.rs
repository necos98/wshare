//! The sessions of a running service: connections still being admitted and
//! viewers admitted, each with its end signal, within [`SessionLimits`].
//! Pure bookkeeping; [`super::state::SharedState`] keeps it under its lock.

use std::collections::HashMap;

use super::state::{Cancel, Cancelled};

/// How many sessions may run at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SessionLimits {
    /// Connections between their arrival and the end of their admission;
    /// more are turned away at once, so silent clients cannot pile up.
    pub(super) max_pending: usize,
    /// Viewers admitted and watching; anyone else is told `Busy`.
    pub(super) max_viewers: usize,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            max_pending: 4,
            max_viewers: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct SessionId(u64);

struct Entry {
    cancel: Cancel,
    admitted: bool,
}

pub(super) struct Sessions {
    limits: SessionLimits,
    next_id: u64,
    entries: HashMap<SessionId, Entry>,
}

impl Sessions {
    pub(super) fn new(limits: SessionLimits) -> Self {
        Self {
            limits,
            next_id: 0,
            entries: HashMap::new(),
        }
    }

    /// Registers a connection to admit, or `None` when too many are being
    /// admitted already.
    pub(super) fn begin(&mut self) -> Option<(SessionId, Cancelled)> {
        let pending = self.entries.values().filter(|e| !e.admitted).count();
        if pending >= self.limits.max_pending {
            return None;
        }
        let id = SessionId(self.next_id);
        self.next_id += 1;
        let (cancel, listener) = Cancel::new();
        self.entries.insert(
            id,
            Entry {
                cancel,
                admitted: false,
            },
        );
        Some((id, listener))
    }

    /// Whether as many viewers as allowed are watching.
    pub(super) fn is_full(&self) -> bool {
        self.viewers() >= self.limits.max_viewers
    }

    /// Makes the connection `id` a viewer, unless that would exceed the
    /// limit (another one was admitted meanwhile) or it was cancelled.
    pub(super) fn admit(&mut self, id: SessionId) -> bool {
        if self.is_full() {
            return false;
        }
        match self.entries.get_mut(&id) {
            Some(entry) if !entry.cancel.is_cancelled() => {
                entry.admitted = true;
                true
            }
            _ => false,
        }
    }

    pub(super) fn end(&mut self, id: SessionId) {
        self.entries.remove(&id);
    }

    /// Asks every admitted viewer to end; admissions go on.
    pub(super) fn cancel_viewers(&self) {
        self.entries
            .values()
            .filter(|e| e.admitted)
            .for_each(|e| e.cancel.cancel());
    }

    /// Asks every session, admitted or not, to end.
    pub(super) fn cancel_all(&self) {
        self.entries.values().for_each(|e| e.cancel.cancel());
    }

    fn viewers(&self) -> usize {
        self.entries.values().filter(|e| e.admitted).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions(max_pending: usize, max_viewers: usize) -> Sessions {
        Sessions::new(SessionLimits {
            max_pending,
            max_viewers,
        })
    }

    #[test]
    fn one_viewer_at_a_time_by_default() {
        assert_eq!(SessionLimits::default().max_viewers, 1);
        let mut s = Sessions::new(SessionLimits::default());
        let (first, _) = s.begin().unwrap();
        let (second, _) = s.begin().unwrap();
        assert!(!s.is_full());
        assert!(s.admit(first));
        assert!(s.is_full());
        assert!(!s.admit(second), "the race for the last place is lost");
        s.end(first);
        assert!(!s.is_full());
        assert!(s.admit(second));
    }

    #[test]
    fn the_viewer_limit_is_a_parameter() {
        let mut s = sessions(4, 2);
        let ids: Vec<SessionId> = (0..3).map(|_| s.begin().unwrap().0).collect();
        assert!(s.admit(ids[0]));
        assert!(s.admit(ids[1]));
        assert!(!s.admit(ids[2]));
    }

    #[test]
    fn pending_admissions_are_capped_but_viewers_do_not_count() {
        let mut s = sessions(2, 1);
        let (first, _) = s.begin().unwrap();
        let (_second, _) = s.begin().unwrap();
        assert!(s.begin().is_none(), "over the cap");
        assert!(s.admit(first));
        assert!(s.begin().is_some(), "an admitted viewer frees its place");
    }

    #[test]
    fn ids_are_not_reused() {
        let mut s = sessions(1, 1);
        let (first, _) = s.begin().unwrap();
        s.end(first);
        let (second, _) = s.begin().unwrap();
        assert_ne!(first, second);
        s.end(first);
        assert!(s.admit(second), "ending a stale id leaves others alone");
    }

    #[test]
    fn ending_viewers_leaves_admissions_running() {
        let mut s = sessions(4, 1);
        let (viewer, viewer_signal) = s.begin().unwrap();
        let (_pending, pending_signal) = s.begin().unwrap();
        assert!(s.admit(viewer));
        s.cancel_viewers();
        assert!(viewer_signal.is_cancelled());
        assert!(!pending_signal.is_cancelled());
        s.cancel_all();
        assert!(pending_signal.is_cancelled());
    }

    #[test]
    fn a_cancelled_admission_cannot_become_a_viewer() {
        let mut s = sessions(4, 1);
        let (id, _) = s.begin().unwrap();
        s.cancel_all();
        assert!(!s.admit(id));
        assert!(!s.is_full());
    }
}
