//! The viewer processes that are running, so one can be ended while its
//! connection is still being made. The thread that reads a viewer's output
//! takes the process back when the output ends; cancelling takes it first.

use std::collections::HashMap;
use std::io::{self, Write};
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

    /// Writes `line` (and a line break) to the process's input.
    pub fn write_line(&self, id: ViewerId, line: &str) -> io::Result<()> {
        let mut running = self.lock();
        let input = running
            .get_mut(&id)
            .and_then(|child| child.stdin.as_mut())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the viewer is not running"))?;
        writeln!(input, "{line}")?;
        input.flush()
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
    fn a_line_reaches_the_input_of_a_running_process() {
        use std::io::Read;

        // findstr echoes every line of its input that matches.
        let mut child = Command::new("findstr")
            .arg(".*")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("findstr.exe is part of Windows");
        let mut output = child.stdout.take().unwrap();
        let processes = ViewerProcesses::default();
        processes.insert(3, child);
        processes.write_line(3, "@wshare:cancel-transfers").unwrap();
        let mut child = processes.take(3).unwrap();
        drop(child.stdin.take());
        let mut echoed = String::new();
        output.read_to_string(&mut echoed).unwrap();
        assert_eq!(echoed.trim_end(), "@wshare:cancel-transfers");
        child.wait().unwrap();
    }

    #[test]
    fn writing_to_a_viewer_not_running_fails() {
        assert!(ViewerProcesses::default().write_line(9, "x").is_err());
    }

    #[test]
    fn unknown_ids_are_ignored() {
        assert!(ViewerProcesses::default().terminate(9).is_none());
    }
}
