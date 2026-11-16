//! [`Updates`] backed by the GitHub Releases of wshare (see `update`):
//! checks and installations run on threads of their own, since downloads
//! take a while.
//!
//! The paths are taken when the window starts: once an update renames the
//! running exe to `wshare.exe.old`, the new program is the one still named
//! `wshare.exe`.

use std::path::PathBuf;
use std::process::Command;
use std::thread;

use anyhow::{Context, Result};

use crate::unattended;
use crate::update::{self, Available, PROGRAM_FILES};

use super::ports::{AppEvent, Inbox, UpdateEvent, Updates};

pub struct GithubUpdates {
    inbox: Inbox,
    /// The folder of the running wshare.exe; `None` if it cannot be told.
    dir: Option<PathBuf>,
}

impl GithubUpdates {
    /// Also removes what a previous update left in the program's folder.
    pub fn new(inbox: Inbox) -> Self {
        let dir = update::program_dir().ok();
        if let Some(dir) = &dir {
            update::clean_leftovers(dir);
        }
        Self { inbox, dir }
    }

    fn exe(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|dir| dir.join(PROGRAM_FILES[0]))
    }

    fn spawn(&self, name: &str, work: impl FnOnce() -> UpdateEvent + Send + 'static) {
        let inbox = self.inbox.clone();
        let spawned = thread::Builder::new()
            .name(name.into())
            .spawn(move || inbox.send(AppEvent::Update(work())));
        if let Err(e) = spawned {
            let failure = update::UpdateFailure::Install(e.to_string());
            let event = if name.ends_with("install") {
                UpdateEvent::Installed(Err(failure))
            } else {
                UpdateEvent::Checked(Err(failure))
            };
            self.inbox.send(AppEvent::Update(event));
        }
    }
}

impl Updates for GithubUpdates {
    fn check(&self) {
        self.spawn("wshare-update-check", || {
            UpdateEvent::Checked(update::check())
        });
    }

    fn install(&self, available: Available) {
        let dir = self.dir.clone();
        self.spawn("wshare-update-install", move || {
            let result = match dir {
                Some(dir) => update::install(&available, &dir),
                None => Err(update::UpdateFailure::Install(
                    "the folder of wshare.exe is unknown".into(),
                )),
            };
            UpdateEvent::Installed(result)
        });
    }

    fn restart(&self) -> Result<()> {
        let exe = self.exe().context("the folder of wshare.exe is unknown")?;
        Command::new(&exe)
            .spawn()
            .with_context(|| format!("starting {}", exe.display()))?;
        Ok(())
    }

    fn service_outdated(&self) -> bool {
        let (Some(exe), Ok(installed)) = (self.exe(), unattended::installed_exe()) else {
            return false;
        };
        // No copy: the service is not installed. Unreadable: cannot tell.
        match (std::fs::read(&exe), std::fs::read(&installed)) {
            (Ok(ours), Ok(theirs)) => ours != theirs,
            _ => false,
        }
    }
}
