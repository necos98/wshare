//! wshare — LAN screen sharing. See `cli::USAGE`.

mod bench;
mod cli;
mod handshake;
mod host;
mod paths;
mod target;
mod viewer;
mod wire;

use std::time::Duration;

use anyhow::Result;
use cli::Command;

/// Per-monitor DPI awareness (needed so capture sees the real resolution)
/// is declared in the embedded manifest, see build.rs.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = cli::parse(&args).and_then(run) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::Monitors => list_monitors(),
        Command::List => list_hosts(),
        Command::Bench(options) => bench::run(options),
        Command::Host(options) => host::run(options),
        Command::View(options) => viewer::run(options),
        Command::Help => {
            println!("{}", cli::USAGE);
            Ok(())
        }
    }
}

fn list_monitors() -> Result<()> {
    for m in capture::list_monitors()? {
        println!("[{}] {} {}x{} on {}", m.index, m.name, m.width, m.height, m.adapter_name);
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
