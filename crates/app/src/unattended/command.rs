//! The commands of unattended access, as text. Pure, unit-tested:
//! - `wshare service ...` on the command line ([`parse`]);
//! - the requests the agent sends the service on its standard output, one
//!   per line ([`AgentRequest`]).

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use super::password::PasswordHash;
use crate::cli::for_each_arg;
use crate::options::DEFAULT_PORT;

/// `wshare service <what>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceCommand {
    /// Installs (or updates) the service and starts it. Needs an
    /// administrator.
    Install(InstallOptions),
    /// Stops and removes the service and everything it installed.
    Uninstall,
    /// Says whether the service is installed and running.
    Status,
    /// Started by Windows: the service itself.
    Run,
    /// Started by the service in the session at the console: the host.
    Agent,
}

/// What `install` sets up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOptions {
    pub port: u16,
    pub password: PasswordChoice,
    /// The data folder whose identity and authorized devices the service
    /// takes over; `None`: the one of the user running the command.
    pub from: Option<PathBuf>,
}

/// The password unattended access will ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordChoice {
    None,
    /// Asked on the terminal.
    Prompt,
    /// Already hashed (by the desktop interface, which never hands the
    /// password itself to another process).
    Hash(PasswordHash),
}

/// The words after `wshare service`.
pub fn parse(args: &[String]) -> Result<ServiceCommand> {
    let Some((what, rest)) = args.split_first() else {
        bail!("service needs a command: install, uninstall or status");
    };
    let command = match what.as_str() {
        "install" => return parse_install(rest).map(ServiceCommand::Install),
        "uninstall" => ServiceCommand::Uninstall,
        "status" => ServiceCommand::Status,
        "run" => ServiceCommand::Run,
        "agent" => ServiceCommand::Agent,
        other => bail!("unknown service command '{other}'"),
    };
    match rest.first() {
        Some(arg) => bail!("unexpected argument '{arg}'"),
        None => Ok(command),
    }
}

fn parse_install(args: &[String]) -> Result<InstallOptions> {
    let mut options = InstallOptions {
        port: DEFAULT_PORT,
        password: PasswordChoice::None,
        from: None,
    };
    for_each_arg(args, |flag, value| {
        let password = match flag {
            "--port" => {
                options.port = value()?.parse().context("--port")?;
                if options.port == 0 {
                    bail!("--port must be between 1 and 65535");
                }
                return Ok(true);
            }
            "--from" => {
                options.from = Some(PathBuf::from(value()?));
                return Ok(true);
            }
            "--password" => PasswordChoice::Prompt,
            "--password-hash" => PasswordChoice::Hash(value()?.parse()?),
            _ => return Ok(false),
        };
        if options.password != PasswordChoice::None {
            bail!("give the password only once");
        }
        options.password = password;
        Ok(true)
    })?;
    Ok(options)
}

/// What the agent asks of the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRequest {
    /// Generate Ctrl+Alt+Del (only a service may).
    SecureAttention,
}

const SECURE_ATTENTION_LINE: &str = "secure-attention";

impl AgentRequest {
    /// The line the agent writes, without the line break.
    pub fn to_line(self) -> &'static str {
        match self {
            Self::SecureAttention => SECURE_ATTENTION_LINE,
        }
    }

    /// The request in a line the agent wrote; `None` for anything else.
    pub fn parse(line: &str) -> Option<Self> {
        match line.trim_end() {
            SECURE_ATTENTION_LINE => Some(Self::SecureAttention),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    fn install(line: &str) -> InstallOptions {
        match parse(&args(line)).unwrap() {
            ServiceCommand::Install(options) => options,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn simple_commands() {
        for (line, command) in [
            ("uninstall", ServiceCommand::Uninstall),
            ("status", ServiceCommand::Status),
            ("run", ServiceCommand::Run),
            ("agent", ServiceCommand::Agent),
        ] {
            assert_eq!(parse(&args(line)).unwrap(), command, "{line}");
            assert!(parse(&args(&format!("{line} extra"))).is_err(), "{line}");
        }
    }

    #[test]
    fn install_defaults_to_no_password_on_the_usual_port() {
        assert_eq!(
            install("install"),
            InstallOptions {
                port: DEFAULT_PORT,
                password: PasswordChoice::None,
                from: None,
            }
        );
    }

    #[test]
    fn install_with_every_option() {
        let options = install("install --port 9000 --password --from C:/Users/anna/wshare");
        assert_eq!(options.port, 9000);
        assert_eq!(options.password, PasswordChoice::Prompt);
        assert_eq!(options.from, Some(PathBuf::from("C:/Users/anna/wshare")));
    }

    #[test]
    fn install_takes_a_password_hash_but_checks_it() {
        let hash = format!(
            "pbkdf2-sha256$600000${}${}",
            "ab".repeat(16),
            "cd".repeat(32)
        );
        let options = install(&format!("install --password-hash {hash}"));
        assert_eq!(
            options.password,
            PasswordChoice::Hash(hash.parse().unwrap())
        );
        assert!(parse(&args("install --password-hash segreto")).is_err());
        assert!(parse(&args("install --password-hash")).is_err());
    }

    #[test]
    fn invalid_commands_are_errors() {
        for line in [
            "",
            "start",
            "install --port 0",
            "install --port 70000",
            "install --port",
            "install --from",
            "install --password --password",
            "install --bogus",
            "install extra",
        ] {
            assert!(parse(&args(line)).is_err(), "{line:?} should fail");
        }
    }

    #[test]
    fn agent_requests_survive_their_line() {
        let request = AgentRequest::SecureAttention;
        assert_eq!(AgentRequest::parse(request.to_line()), Some(request));
        assert_eq!(
            AgentRequest::parse(&format!("{}\r\n", request.to_line())),
            Some(request)
        );
    }

    #[test]
    fn other_lines_are_not_requests() {
        for line in [
            "",
            "secure",
            " secure-attention",
            "SECURE-ATTENTION",
            "reboot",
        ] {
            assert_eq!(AgentRequest::parse(line), None, "{line:?}");
        }
    }
}
