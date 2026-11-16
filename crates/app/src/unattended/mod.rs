//! Unattended access: using this PC from the LAN with nobody in front of
//! it, including the lock screen, the sign-in screen, the UAC prompts and
//! Ctrl+Alt+Del. Off by default; turning it on needs an administrator.
//!
//! Three processes, all `wshare.exe` (the README draws the picture):
//! - the **service** (`wshare service run`, started by Windows as
//!   LocalSystem, see `service_main`): keeps one agent running in the
//!   session at the console, restarts it when that session changes or the
//!   agent ends (`supervisor`, `agent_process`), and generates
//!   Ctrl+Alt+Del when the agent asks (`secure_attention`);
//! - the **agent** (`wshare service agent`, see `agent`): the host itself,
//!   running as SYSTEM in that session. Its capture and input threads
//!   follow the input desktop (`input_desktop` crate), so they reach the
//!   secure desktop too. It admits only authorized devices, with the
//!   password if one is set (`rules`), and shows the connection indicator;
//! - the **desktop interface**, which no longer shares this PC itself while
//!   the service is installed: the service took over (it would hold the
//!   same port). It installs and removes the service through an elevated
//!   `wshare service install|uninstall` (`elevation`).
//!
//! `install` copies wshare.exe to Program Files (a copy only
//! administrators can replace, since it runs as SYSTEM) and the user's
//! identity and authorized devices to a folder only SYSTEM and
//! administrators can read (`locations`, `protected_dir`), so viewers that
//! know this PC keep connecting to the same identity. `uninstall` removes
//! the service, both folders and nothing else is left: the only registry
//! value touched (`SoftwareSASGeneration`) is restored right after each
//! Ctrl+Alt+Del.

mod agent;
mod agent_process;
mod command;
mod config;
mod elevation;
mod install;
mod locations;
mod password;
mod password_prompt;
mod protected_dir;
mod rules;
mod scm;
mod secure_attention;
mod service_log;
mod service_main;
mod service_settings;
mod supervisor;

pub use command::{ServiceCommand, parse};
pub use elevation::{ElevatedOutcome, installed_state, run_elevated};
pub use password::{MAX_PASSWORD_CHARS, PasswordHash};
pub use rules::{PasswordCheck, needs_password_check, verdict};
pub use service_settings::ServiceSettings;

use anyhow::Result;

/// The service name Windows knows it by (`sc query wshare`).
pub const SERVICE_NAME: &str = "wshare";

/// How the host of the agent admits viewers (see [`rules`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnattendedPolicy {
    /// `None`: an authorized certificate is enough.
    pub password: Option<PasswordHash>,
}

/// The copy of wshare.exe the service runs (it may not exist).
pub fn installed_exe() -> Result<std::path::PathBuf> {
    Ok(locations::installed_exe(&locations::program_dir()?))
}

/// `wshare service <command>`.
pub fn run(command: ServiceCommand) -> Result<()> {
    match command {
        ServiceCommand::Install(options) => install::install(&options),
        ServiceCommand::Uninstall => install::uninstall(),
        ServiceCommand::Status => install::print_status(),
        ServiceCommand::Run => service_main::run(),
        ServiceCommand::Agent => agent::run(),
    }
}
