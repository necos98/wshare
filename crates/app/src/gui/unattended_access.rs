//! [`UnattendedAccess`] backed by the real service: the state is read from
//! Windows, and turning access on or off runs `wshare service install` or
//! `uninstall` as an administrator, on a thread of its own (Windows asks
//! the user first, and the password is hashed: both take a while).
//!
//! The password itself never leaves this process: the elevated command
//! gets its salted hash, with the host settings the service applies.

use std::thread;

use anyhow::Result;

use crate::paths;
use crate::text;
use crate::unattended::{self, ElevatedOutcome, PasswordHash, ServiceSettings};

use super::ports::{AppEvent, Inbox, UnattendedAccess, UnattendedOutcome};

pub struct WindowsUnattended {
    inbox: Inbox,
}

impl WindowsUnattended {
    pub fn new(inbox: Inbox) -> Self {
        Self { inbox }
    }

    /// Runs, as an administrator, the command `args` builds; reports how
    /// it ended.
    fn run(&self, args: impl FnOnce() -> Result<Vec<String>> + Send + 'static) {
        let inbox = self.inbox.clone();
        let work = move || {
            let outcome = match args().and_then(|args| unattended::run_elevated(&args)) {
                Ok(ElevatedOutcome::Succeeded) => UnattendedOutcome::Done,
                Ok(ElevatedOutcome::Cancelled) => UnattendedOutcome::Cancelled,
                Ok(ElevatedOutcome::Failed(code)) => {
                    UnattendedOutcome::Failed(text::unattended_exit_code(code))
                }
                Err(e) => UnattendedOutcome::Failed(format!("{e:#}")),
            };
            inbox.send(AppEvent::Unattended(outcome));
        };
        let spawned = thread::Builder::new()
            .name("wshare-unattended".into())
            .spawn(work);
        if let Err(e) = spawned {
            self.inbox
                .send(AppEvent::Unattended(UnattendedOutcome::Failed(
                    e.to_string(),
                )));
        }
    }
}

impl UnattendedAccess for WindowsUnattended {
    fn installed(&self) -> Result<bool> {
        Ok(unattended::installed_state()?.installed())
    }

    fn install(&self, settings: ServiceSettings, password: Option<String>) {
        self.run(move || install_args(&settings, password.as_deref()));
    }

    fn uninstall(&self) {
        self.run(|| Ok(vec!["service".into(), "uninstall".into()]));
    }
}

/// `service install` with the host settings the service applies, taking
/// over this user's identity and authorized devices (the administrator who
/// confirms may be another user).
fn install_args(settings: &ServiceSettings, password: Option<&str>) -> Result<Vec<String>> {
    let mut args = vec!["service".to_string(), "install".to_string()];
    args.extend(settings.install_args());
    args.push("--from".to_string());
    args.push(paths::config_dir()?.display().to_string());
    if let Some(password) = password {
        args.push("--password-hash".to_string());
        args.push(PasswordHash::new(password)?.to_string());
    }
    Ok(args)
}
