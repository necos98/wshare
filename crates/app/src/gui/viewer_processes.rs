//! The viewer processes that are running, so one can be ended while its
//! connection is still being made. The thread that reads a viewer's output
//! takes the process back when the output ends; cancelling takes it first.

use std::collections::HashMap;
use std::process::{Child, ExitStatus};
use std::sync::{Arc, Mutex, MutexGuard};

use super::model::ViewerId;

#[derive(Clone, Default)]
pub struct ViewerProcesses {
    running: Arc<Mutex<HashMap<ViewerId, Child>>>,
}

impl ViewerProcesses {
    pub fn insert(&self, id: ViewerId, child: Child) {
        self.lock().insert(id, child);
    }

    /// Hands the process back to whoever waits for its end; `None` if it was
    /// cancelled meanwhile.
    pub fn take(&self, id: ViewerId) -> Option<Child> {
        self.lock().remove(&id)
    }

    /// Ends the process and collects it; `None` if it is not running here
    /// (it ended on its own, or was already cancelled).
    pub fn terminate(&self, id: ViewerId) -> Option<ExitStatus> {
        let mut child = self.take(id)?;
        // Failing means it had just ended: either way it is over.
        let _ = child.kill();
        child.wait().ok()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<ViewerId, Child>> {
        self.running.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    /// A process that would run for a minute.
    fn long_running() -> Child {
        Command::new("cmd")
            .args(["/C", "ping -n 60 127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("cmd.exe is part of Windows")
    }

    #[test]
    fn terminating_ends_a_running_process_once() {
        let processes = ViewerProcesses::default();
        processes.insert(1, long_running());
        let status = processes.terminate(1).expect("it was running");
        assert!(!status.success());
        assert!(processes.terminate(1).is_none(), "nothing left to end");
    }

    #[test]
    fn a_taken_process_cannot_be_terminated() {
        let processes = ViewerProcesses::default();
        processes.insert(2, long_running());
        let mut child = processes.take(2).expect("it was running");
        assert!(processes.terminate(2).is_none());
        assert!(processes.take(2).is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn unknown_ids_are_ignored() {
        assert!(ViewerProcesses::default().terminate(9).is_none());
    }
}
