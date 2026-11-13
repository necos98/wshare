//! Command-line parsing. Pure: no I/O, fully unit-tested.

use anyhow::{Context, Result, bail};
use codec::Codec;

pub const DEFAULT_PORT: u16 = 7420;

/// Encoding settings shared by `host` and `bench`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOptions {
    pub monitor: usize,
    pub fps: u32,
    pub mbps: u32,
    pub codec: Codec,
}

impl StreamOptions {
    pub fn encoder_config(&self, width: u32, height: u32) -> codec::EncoderConfig {
        codec::EncoderConfig {
            codec: self.codec,
            width,
            height,
            fps: self.fps,
            bitrate_bps: self.mbps.saturating_mul(1_000_000),
        }
    }
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            monitor: 0,
            fps: 60,
            mbps: 20,
            codec: Codec::H264,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchOptions {
    pub stream: StreamOptions,
    pub seconds: u64,
    pub out: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostOptions {
    pub stream: StreamOptions,
    pub port: u16,
    /// Fixed PIN; a random one is generated when absent.
    pub pin: Option<String>,
    /// Refuse remote mouse/keyboard: viewers can only watch.
    pub view_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewOptions {
    /// Address, address:port, or (part of) a host name. `None`: discover.
    pub target: Option<String>,
    pub pin: Option<String>,
    /// Only watch: do not send mouse/keyboard even if the host allows it.
    pub view_only: bool,
    /// Open the viewer window in full screen (toggle with Ctrl+Alt+Enter).
    pub fullscreen: bool,
    /// Show the statistics overlay from the start (toggle with Ctrl+Alt+I).
    pub stats: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Monitors,
    List,
    Bench(BenchOptions),
    Host(HostOptions),
    View(ViewOptions),
    Help,
    /// No arguments (e.g. double-click): the desktop interface.
    Gui,
}

pub const USAGE: &str = "\
wshare - LAN screen sharing (no arguments: open the window)

  wshare host  [--monitor N] [--fps F] [--mbps B] [--codec h264|hevc] [--port P] [--pin PIN]
               [--view-only]
  wshare view  [HOST | IP[:PORT]] [--pin PIN] [--view-only] [--fullscreen] [--stats]
               in the viewer: Ctrl+Alt+Enter full screen, Ctrl+Alt+I statistics
  wshare list                       hosts found on the LAN
  wshare monitors                   monitors of this PC
  wshare bench [--monitor N] [--fps F] [--mbps B] [--codec h264|hevc] [--seconds S] [--out FILE]";

pub fn parse(args: &[String]) -> Result<Command> {
    let Some((command, rest)) = args.split_first() else {
        return Ok(Command::Gui);
    };
    match command.as_str() {
        "monitors" => no_args(rest, Command::Monitors),
        "list" => no_args(rest, Command::List),
        "bench" => parse_bench(rest).map(Command::Bench),
        "host" => parse_host(rest).map(Command::Host),
        "view" => parse_view(rest).map(Command::View),
        "help" | "--help" | "-h" => Ok(Command::Help),
        other => bail!("unknown command '{other}'"),
    }
}

fn no_args(rest: &[String], command: Command) -> Result<Command> {
    match rest.first() {
        Some(arg) => bail!("unexpected argument '{arg}'"),
        None => Ok(command),
    }
}

/// Walks `--flag value` pairs and bare positionals, delegating each to `on`.
fn for_each_arg(
    args: &[String],
    mut on: impl FnMut(&str, &mut dyn FnMut() -> Result<String>) -> Result<bool>,
) -> Result<()> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        if !on(arg, &mut value)? {
            bail!("unknown option '{arg}'");
        }
    }
    Ok(())
}

/// Handles a flag belonging to [`StreamOptions`]; returns false if it isn't one.
fn stream_flag(
    stream: &mut StreamOptions,
    flag: &str,
    value: &mut dyn FnMut() -> Result<String>,
) -> Result<bool> {
    match flag {
        "--monitor" => stream.monitor = value()?.parse().context("--monitor")?,
        "--fps" => stream.fps = positive(&value()?, "--fps")?,
        "--mbps" => stream.mbps = positive(&value()?, "--mbps")?,
        "--codec" => stream.codec = parse_codec(&value()?)?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn parse_bench(args: &[String]) -> Result<BenchOptions> {
    let mut options = BenchOptions {
        stream: StreamOptions::default(),
        seconds: 10,
        out: None,
    };
    for_each_arg(args, |flag, value| {
        if stream_flag(&mut options.stream, flag, value)? {
            return Ok(true);
        }
        match flag {
            "--seconds" => options.seconds = value()?.parse().context("--seconds")?,
            "--out" => options.out = Some(value()?),
            _ => return Ok(false),
        }
        Ok(true)
    })?;
    Ok(options)
}

fn parse_host(args: &[String]) -> Result<HostOptions> {
    let mut options = HostOptions {
        stream: StreamOptions::default(),
        port: DEFAULT_PORT,
        pin: None,
        view_only: false,
    };
    for_each_arg(args, |flag, value| {
        if stream_flag(&mut options.stream, flag, value)? {
            return Ok(true);
        }
        match flag {
            "--port" => options.port = value()?.parse().context("--port")?,
            "--pin" => options.pin = Some(value()?),
            "--view-only" => options.view_only = true,
            _ => return Ok(false),
        }
        Ok(true)
    })?;
    Ok(options)
}

fn parse_view(args: &[String]) -> Result<ViewOptions> {
    let mut options = ViewOptions {
        target: None,
        pin: None,
        view_only: false,
        fullscreen: false,
        stats: false,
    };
    for_each_arg(args, |arg, value| {
        match arg {
            "--pin" => options.pin = Some(value()?),
            "--view-only" => options.view_only = true,
            "--fullscreen" => options.fullscreen = true,
            "--stats" => options.stats = true,
            positional if !positional.starts_with("--") && options.target.is_none() => {
                options.target = Some(positional.to_string())
            }
            _ => return Ok(false),
        }
        Ok(true)
    })?;
    Ok(options)
}

fn parse_codec(value: &str) -> Result<Codec> {
    match value.to_ascii_lowercase().as_str() {
        "h264" | "avc" => Ok(Codec::H264),
        "hevc" | "h265" => Ok(Codec::Hevc),
        other => bail!("unknown codec '{other}' (use h264 or hevc)"),
    }
}

fn positive(value: &str, flag: &str) -> Result<u32> {
    match value.parse::<u32>() {
        Ok(n) if n > 0 => Ok(n),
        _ => bail!("{flag} must be a positive integer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn no_arguments_opens_the_interface() {
        assert_eq!(parse(&[]).unwrap(), Command::Gui);
    }

    #[test]
    fn help_still_prints_usage() {
        for line in ["help", "--help", "-h"] {
            assert_eq!(parse(&args(line)).unwrap(), Command::Help, "{line}");
        }
    }

    #[test]
    fn host_defaults() {
        let Command::Host(h) = parse(&args("host")).unwrap() else {
            panic!()
        };
        assert_eq!(h.stream, StreamOptions::default());
        assert_eq!(h.port, DEFAULT_PORT);
        assert_eq!(h.pin, None);
        assert!(!h.view_only);
    }

    #[test]
    fn host_with_every_option() {
        let line =
            "host --monitor 1 --fps 30 --mbps 50 --codec HEVC --port 9000 --pin 123456 --view-only";
        let Command::Host(h) = parse(&args(line)).unwrap() else {
            panic!()
        };
        assert_eq!(
            h.stream,
            StreamOptions {
                monitor: 1,
                fps: 30,
                mbps: 50,
                codec: Codec::Hevc
            }
        );
        assert_eq!(h.port, 9000);
        assert_eq!(h.pin.as_deref(), Some("123456"));
        assert!(h.view_only);
    }

    #[test]
    fn view_with_target_and_pin_in_any_order() {
        let expected = ViewOptions {
            target: Some("192.168.1.20:7420".into()),
            pin: Some("42".into()),
            view_only: false,
            fullscreen: false,
            stats: false,
        };
        let Command::View(a) = parse(&args("view 192.168.1.20:7420 --pin 42")).unwrap() else {
            panic!()
        };
        let Command::View(b) = parse(&args("view --pin 42 192.168.1.20:7420")).unwrap() else {
            panic!()
        };
        assert_eq!(a, expected);
        assert_eq!(b, expected);
    }

    #[test]
    fn view_without_target_discovers() {
        let Command::View(v) = parse(&args("view")).unwrap() else {
            panic!()
        };
        assert_eq!(v.target, None);
    }

    #[test]
    fn view_only_viewer() {
        let Command::View(v) = parse(&args("view laptop --view-only")).unwrap() else {
            panic!()
        };
        assert!(v.view_only);
        assert_eq!(v.target.as_deref(), Some("laptop"));
        assert!(!v.fullscreen);
    }

    #[test]
    fn fullscreen_viewer() {
        let Command::View(v) = parse(&args("view --fullscreen --stats")).unwrap() else {
            panic!()
        };
        assert!(v.fullscreen);
        assert!(v.stats);
    }

    #[test]
    fn view_rejects_two_targets() {
        assert!(parse(&args("view a b")).is_err());
    }

    #[test]
    fn bench_options() {
        let Command::Bench(b) = parse(&args("bench --seconds 3 --out x.h264")).unwrap() else {
            panic!()
        };
        assert_eq!(b.seconds, 3);
        assert_eq!(b.out.as_deref(), Some("x.h264"));
    }

    #[test]
    fn invalid_values_are_errors() {
        for line in [
            "host --fps 0",
            "host --fps",
            "host --codec vp9",
            "host --port 99999",
            "host --bogus",
            "list extra",
            "frobnicate",
        ] {
            assert!(parse(&args(line)).is_err(), "{line} should fail");
        }
    }
}
