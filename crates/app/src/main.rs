//! wshare — LAN screen sharing. See `cli::USAGE`.
//!
//! A "windows" subsystem program: double-clicking opens the interface
//! without a console window; command-line commands attach to the console
//! of the terminal that started them (see `gui::platform`).

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod access_mode;
mod bench;
mod cli;
mod clipboard;
mod data_migration;
mod desktop;
mod gui;
mod handshake;
mod host;
mod options;
mod paths;
mod printable;
mod realtime;
mod target;
mod text;
mod transfer;
mod trust;
mod unattended;
mod viewer;
mod wire;

use std::time::Duration;

use anyhow::Result;
use cli::Command;

/// Per-monitor DPI awareness (needed so capture sees the real resolution)
/// is declared in the embedded manifest, see build.rs.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = cli::parse(&args);
    let is_gui = matches!(command, Ok(Command::Gui(_)));
    let is_view = matches!(command, Ok(Command::View(_)));
    if !is_gui {
        gui::platform::attach_parent_console();
    }
    if let Err(e) = command.and_then(run) {
        if is_gui {
            gui::platform::error_box(&text::window_failed(&format!("{e:#}")));
        } else {
            eprintln!("error: {e:#}");
        }
        // The desktop interface reads why a viewer failed from its code.
        let code = if is_view {
            viewer::exit_code_for(&e)
        } else {
            viewer::report::GENERIC_EXIT_CODE
        };
        std::process::exit(code);
    }
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::Monitors => list_monitors(),
        Command::List => list_hosts(),
        Command::Bench(options) => bench::run(options),
        Command::Host(options) => host::run(options),
        Command::View(options) => viewer::run(options),
        Command::Gui(options) => gui::run(options),
        Command::Service(command) => unattended::run(command),
        Command::Help => {
            println!("{}", cli::USAGE);
            Ok(())
        }
    }
}

fn list_monitors() -> Result<()> {
    for m in capture::list_monitors()? {
        println!(
            "[{}] {} {}x{} on {}",
            m.index, m.name, m.width, m.height, m.adapter_name
        );
    }
    Ok(())
}

fn list_hosts() -> Result<()> {
    println!("Searching the LAN for 3 s...");
    let hosts = target::discover(Duration::from_secs(3))?;
    if hosts.is_empty() {
        println!("No hosts found. Is `wshare host` running, and UDP 5353 allowed by the firewall?");
    }
    for host in hosts.hosts() {
        let a = &host.announcement;
        let address = host
            .preferred_address()
            .map_or_else(|| "no address".to_string(), |s| s.to_string());
        println!("{}  {}x{}  {address}", a.display_name, a.width, a.height);
    }
    Ok(())
}
