//! [`ViewerLauncher`] that runs each viewer as a child process
//! (`wshare.exe view <ip:port> [--host-id ID]`): winit allows a single
//! event loop per process and the viewer renders with its own swap chain.
//!
//! The PIN travels in the `WSHARE_PIN` environment variable, not on the
//! command line (visible to every process of the machine); without one the
//! variable is removed, so the viewer never inherits a stale PIN.
//! `WSHARE_REPORT` asks the viewer for its status lines on stdout and the
//! exit code (see `viewer::report`): the interface decides only from those,
//! never from the wording of messages. The viewer's file transfers travel
//! as typed lines too (see `viewer::channel`): their events on stdout, the
//! interface's commands on stdin.

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::transfer::TransferCommand;
use crate::viewer::PIN_ENV;
use crate::viewer::channel;
use crate::viewer::report::{FailureCause, REPORT_ENABLED, REPORT_ENV, ViewerReport};

use super::model::{ViewerId, ViewerUpdate};
use super::ports::{AppEvent, Inbox, LaunchRequest, ViewerLauncher};
use super::viewer_processes::ViewerProcesses;

/// `CREATE_NO_WINDOW`: no console for the child, whatever its subsystem.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct ProcessLauncher {
    exe: PathBuf,
    inbox: Inbox,
    processes: ViewerProcesses,
}

impl ProcessLauncher {
    pub fn new(inbox: Inbox) -> Result<Self> {
        let exe = std::env::current_exe().context("locating wshare.exe")?;
        Ok(Self {
            exe,
            inbox,
            processes: ViewerProcesses::default(),
        })
    }
}

impl ViewerLauncher for ProcessLauncher {
    fn launch(&mut self, id: ViewerId, request: &LaunchRequest) -> Result<()> {
        let mut command = Command::new(&self.exe);
        command.arg("view").arg(request.target.to_string());
        if let Some(host_id) = &request.host_id {
            command.arg("--host-id").arg(host_id);
        }
        command.args(request.display.flags());
        if let Some(downloads) = &request.downloads {
            command.arg("--downloads").arg(downloads);
        }
        match &request.pin {
            Some(pin) => command.env(PIN_ENV, pin),
            None => command.env_remove(PIN_ENV),
        };
        let mut child = command
            .env(REPORT_ENV, REPORT_ENABLED)
            // Commands only: a viewer launched here never reads a PIN there.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .context("starting the viewer")?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("viewer stdout");
        };
        self.processes.insert(id, child);
        let inbox = self.inbox.clone();
        let processes = self.processes.clone();
        std::thread::Builder::new()
            .name(format!("viewer-{id}"))
            .spawn(move || {
                let mut failure = None;
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    match ViewerReport::parse(&line) {
                        Some(ViewerReport::AwaitingApproval) => inbox.send(AppEvent::Viewer {
                            id,
                            update: ViewerUpdate::AwaitingApproval,
                        }),
                        Some(ViewerReport::Connected { .. }) => inbox.send(AppEvent::Viewer {
                            id,
                            update: ViewerUpdate::Connected,
                        }),
                        Some(ViewerReport::Failed(cause)) => failure = Some(cause),
                        None => {
                            if let Some(event) = channel::parse_event(&line) {
                                inbox.send(AppEvent::Viewer {
                                    id,
                                    update: ViewerUpdate::Transfer(event),
                                });
                            }
                        }
                    }
                }
                // Gone from the registry: it was cancelled, and the
                // interface no longer listens for it.
                let status = processes.take(id).map(|mut child| child.wait());
                let success = matches!(&status, Some(Ok(s)) if s.success());
                let code = status.and_then(Result::ok).and_then(|s| s.code());
                inbox.send(AppEvent::Viewer {
                    id,
                    update: exit_update(success, code, failure),
                });
            })?;
        Ok(())
    }

    fn cancel(&mut self, id: ViewerId) {
        self.processes.terminate(id);
    }

    fn command(&mut self, id: ViewerId, command: TransferCommand) -> Result<()> {
        self.processes
            .write_line(id, &channel::command_line(&command))
            .context("telling the viewer")
    }
}

/// What the end of the viewer process means: the cause it reported, else
/// the one its exit code names, else none (killed, crashed).
fn exit_update(success: bool, code: Option<i32>, reported: Option<FailureCause>) -> ViewerUpdate {
    let cause = reported.or_else(|| code.and_then(FailureCause::from_exit_code));
    ViewerUpdate::Exited {
        success,
        code,
        cause,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exited(success: bool, code: Option<i32>, cause: Option<FailureCause>) -> ViewerUpdate {
        ViewerUpdate::Exited {
            success,
            code,
            cause,
        }
    }

    #[test]
    fn the_reported_cause_wins_over_the_exit_code() {
        assert_eq!(
            exit_update(false, Some(1), Some(FailureCause::Other("odd".into()))),
            exited(false, Some(1), Some(FailureCause::Other("odd".into())))
        );
        assert_eq!(
            exit_update(
                false,
                Some(11),
                Some(FailureCause::HostCannotStream("x".into()))
            ),
            exited(
                false,
                Some(11),
                Some(FailureCause::HostCannotStream("x".into()))
            )
        );
    }

    #[test]
    fn without_a_report_the_exit_code_names_the_cause() {
        assert_eq!(
            exit_update(false, Some(11), None),
            exited(false, Some(11), Some(FailureCause::WrongPin))
        );
        assert_eq!(
            exit_update(false, Some(15), None),
            exited(false, Some(15), Some(FailureCause::TimedOut))
        );
    }

    #[test]
    fn a_silent_failure_has_no_cause() {
        assert_eq!(
            exit_update(false, Some(101), None),
            exited(false, Some(101), None)
        );
        assert_eq!(exit_update(false, None, None), exited(false, None, None));
    }

    #[test]
    fn a_normal_end_has_no_cause() {
        assert_eq!(
            exit_update(true, Some(0), None),
            exited(true, Some(0), None)
        );
    }
}
