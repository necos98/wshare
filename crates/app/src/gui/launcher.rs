//! [`ViewerLauncher`] that runs each viewer as a child process
//! (`wshare.exe view <ip:port>`): winit allows a single event loop per
//! process and the viewer renders with its own swap chain.
//!
//! The PIN travels in the `WSHARE_PIN` environment variable, not on the
//! command line (visible to every process of the machine). stdout tells
//! when the viewer connected, stderr why it failed.

use std::io::{BufRead, BufReader, Read};
use std::net::SocketAddr;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::viewer::PIN_ENV;

use super::model::{ViewerId, ViewerUpdate};
use super::ports::{AppEvent, Inbox, ViewerLauncher};

/// `CREATE_NO_WINDOW`: no console for the child, whatever its subsystem.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What `wshare view` prints on stdout once the handshake succeeded.
const CONNECTED_PREFIX: &str = "Connected to";

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
    fn launch(&mut self, id: ViewerId, target: SocketAddr, pin: &str) -> Result<()> {
        let mut child = Command::new(&self.exe)
            .arg("view")
            .arg(target.to_string())
            .env(PIN_ENV, pin)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .context("starting the viewer")?;
        let stdout = child.stdout.take().context("viewer stdout")?;
        let mut stderr = child.stderr.take().context("viewer stderr")?;
        let inbox = self.inbox.clone();
        std::thread::Builder::new().name(format!("viewer-{id}")).spawn(move || {
            let stdout_inbox = inbox.clone();
            let stdout_reader = std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if is_connected_line(&line) {
                        stdout_inbox.send(AppEvent::Viewer { id, update: ViewerUpdate::Connected });
                    }
                }
            });
            let mut errors = Vec::new();
            let _ = stderr.read_to_end(&mut errors);
            let _ = stdout_reader.join();
            let status = child.wait();
            let update = ViewerUpdate::Exited {
                success: status.as_ref().is_ok_and(|s| s.success()),
                code: status.ok().and_then(|s| s.code()),
                stderr: String::from_utf8_lossy(&errors).into_owned(),
            };
            inbox.send(AppEvent::Viewer { id, update });
        })?;
        Ok(())
    }
}

fn is_connected_line(line: &str) -> bool {
    line.starts_with(CONNECTED_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_connected_line() {
        assert!(is_connected_line(
            "Connected to \"UFFICIO\" (10.0.0.2:7420) - 1920x1080 @ 60 fps H264"
        ));
        assert!(!is_connected_line("Host fingerprint: abc"));
    }
}
