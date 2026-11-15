//! [`ViewerLauncher`] that runs each viewer as a child process
//! (`wshare.exe view <ip:port> [--host-id ID]`): winit allows a single
//! event loop per process and the viewer renders with its own swap chain.
//!
//! The PIN travels in the `WSHARE_PIN` environment variable, not on the
//! command line (visible to every process of the machine); without one the
//! variable is removed, so the viewer never inherits a stale PIN.
//! `WSHARE_REPORT` asks the viewer for its status lines on stdout and the
//! exit code (see `viewer::report`): the interface decides only from those,
//! never from the wording of messages.

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::viewer::PIN_ENV;
use crate::viewer::report::{FailureCause, REPORT_ENABLED, REPORT_ENV, ViewerReport};

use super::model::{ViewerId, ViewerUpdate};
use super::ports::{AppEvent, Inbox, LaunchRequest, ViewerLauncher};

/// `CREATE_NO_WINDOW`: no console for the child, whatever its subsystem.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct ProcessLauncher {
    exe: PathBuf,
    inbox: Inbox,
}

impl ProcessLauncher {
    pub fn new(inbox: Inbox) -> Result<Self> {
        let exe = std::env::current_exe().context("locating wshare.exe")?;
        Ok(Self { exe, inbox })
    }
}

impl ViewerLauncher for ProcessLauncher {
    fn launch(&mut self, id: ViewerId, request: &LaunchRequest) -> Result<()> {
        let mut command = Command::new(&self.exe);
        command.arg("view").arg(request.target.to_string());
        if let Some(host_id) = &request.host_id {
            command.arg("--host-id").arg(host_id);
        }
        match &request.pin {
            Some(pin) => command.env(PIN_ENV, pin),
            None => command.env_remove(PIN_ENV),
        };
        let mut child = command
            .env(REPORT_ENV, REPORT_ENABLED)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .context("starting the viewer")?;
        let stdout = child.stdout.take().context("viewer stdout")?;
        let inbox = self.inbox.clone();
        std::thread::Builder::new()
            .name(format!("viewer-{id}"))
            .spawn(move || {
                let mut failure = None;
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    match ViewerReport::parse(&line) {
                        Some(ViewerReport::Connected { .. }) => inbox.send(AppEvent::Viewer {
                            id,
                            update: ViewerUpdate::Connected,
                        }),
                        Some(ViewerReport::Failed(cause)) => failure = Some(cause),
                        None => {}
                    }
                }
                let status = child.wait();
                let update = exit_update(
                    status.as_ref().is_ok_and(|s| s.success()),
                    status.ok().and_then(|s| s.code()),
                    failure,
                );
                inbox.send(AppEvent::Viewer { id, update });
            })?;
        Ok(())
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
