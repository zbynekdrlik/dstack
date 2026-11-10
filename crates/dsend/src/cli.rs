// SPDX-License-Identifier: MIT OR Apache-2.0

//! dsend's command line (dstack #164), parsed by hand as dlisten's is. Pure:
//! the binary reads the process's arguments and environment and passes
//! them in.

use std::ffi::OsString;
use std::net::Ipv4Addr;
use std::path::PathBuf;

use dlink::{Config, Place, Ports};
use dstack_endpoint::cli::{number, parse_ports};

use crate::source::TX_NAMES;

/// The usage text (`--help`, and after a usage error).
pub const USAGE: &str = "\
usage: dsend [options]
Sends this PC's sound into Dante: an output's loopback (what plays on it)
as the TX channels L and R of a Dante device <host>-dsend, converted to
the network's rate and clock. Windows only (WASAPI loopback).

  --list-outputs              list the outputs and exit
  --output <text>             capture the first output whose name holds
                              <text> (any case; default: the default output)
  --name <name>               the device's Dante name (default <host>-dsend)
  --rate <hz>                 the network's sample rate (default 48000)
  --budget-ms <ms>            dlink's latency budget (default 15)
  --ip <addr>                 the Dante NIC's address (default: the address
                              of the default route)
  --iface <name>              the Dante NIC by name (instead of --ip)
  --ports <arc>,<cmc>,<flows> the device's control ports, when another
                              device on this PC holds Dante's defaults
  --process-id <n>            the device's process id, with --ports
  --state <file>              where Dante Controller's settings are kept
  --version, -V               the version
  --help, -h                  this text

dsend has no window: dtray shows it (the status, and an Output menu that
chooses the output to capture, remembered for the next start; --output
wins). Subscribe a receiver to <host>-dsend's L and R in Dante Controller.
The status is also logged every 10 s, to stderr and to <state file>.log.
Ctrl+C stops it.";

/// The network's rate unless `--rate`.
pub const RATE: u32 = 48_000;
/// The name's suffix after the host's name.
pub const SUFFIX: &str = "-dsend";
/// The app's name (its state folder, the command's usage).
pub const APP: &str = "dsend";

/// The flags that take a value.
const VALUE_FLAGS: [&str; 9] = [
    "--output",
    "--name",
    "--rate",
    "--budget-ms",
    "--ip",
    "--iface",
    "--ports",
    "--process-id",
    "--state",
];

/// What a run is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub name: Option<String>,
    pub output: Option<String>,
    pub rate: u32,
    pub budget_ms: Option<u64>,
    /// None: the default route's address.
    pub place: Option<Place>,
    pub ports: Option<Ports>,
    pub process_id: Option<u16>,
    pub state: Option<PathBuf>,
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run(Options),
    ListOutputs,
    Help,
    Version,
}

/// The command line after the program's name.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut o = Options {
        name: None,
        output: None,
        rate: RATE,
        budget_ms: None,
        place: None,
        ports: None,
        process_id: None,
        state: None,
    };
    let mut list = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let flag = arg.as_str();
        match flag {
            "--help" | "-h" => return Ok(Command::Help),
            "--version" | "-V" => return Ok(Command::Version),
            "--list-outputs" => {
                list = true;
                continue;
            }
            _ => {}
        }
        if !VALUE_FLAGS.contains(&flag) {
            return Err(format!("unknown argument {flag:?}"));
        }
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag {
            "--output" if value.is_empty() => return Err("--output needs a name".into()),
            "--output" => o.output = Some(value.clone()),
            "--name" => o.name = Some(value.clone()),
            "--rate" => {
                o.rate = number(flag, value)?;
                if o.rate == 0 {
                    return Err("--rate needs a rate over 0 Hz".into());
                }
            }
            "--budget-ms" => {
                let ms: u64 = number(flag, value)?;
                if ms == 0 {
                    return Err("--budget-ms needs a budget over 0 ms".into());
                }
                o.budget_ms = Some(ms);
            }
            "--ip" | "--iface" if o.place.is_some() => {
                return Err("give one of --ip and --iface, once".into());
            }
            "--ip" => {
                let ip: Ipv4Addr = value
                    .parse()
                    .map_err(|_| format!("--ip needs an IPv4 address, not {value:?}"))?;
                o.place = Some(Place::Ip(ip));
            }
            "--iface" if value.is_empty() => return Err("--iface needs a name".into()),
            "--iface" => o.place = Some(Place::Iface(value.clone())),
            "--ports" => o.ports = Some(parse_ports(value)?),
            "--process-id" => o.process_id = Some(number(flag, value)?),
            "--state" if value.is_empty() => return Err("--state needs a path".into()),
            _ => o.state = Some(PathBuf::from(value)),
        }
    }
    Ok(if list {
        Command::ListOutputs
    } else {
        Command::Run(o)
    })
}

/// Whether this OS can capture an output's sound: Windows only (WASAPI
/// loopback; `windows` is `cfg!(windows)`). The error says why and what
/// works instead.
pub fn capture_check(windows: bool) -> Result<(), String> {
    if windows {
        Ok(())
    } else {
        Err(
            "dsend captures an output's sound by WASAPI loopback, which only Windows has \
             (on Linux an output's monitor is PulseAudio's or PipeWire's, not ALSA's): \
             run dsend on Windows"
                .into(),
        )
    }
}

/// The default device name for a host named `host`: `<host>-dsend`
/// ([`dstack_endpoint::cli::default_name`]).
pub fn default_name(host: &str) -> String {
    dstack_endpoint::cli::default_name(host, SUFFIX)
}

/// Where the device keeps what Dante Controller sets when `--state` is not
/// given: `.../dstack/dsend/<name>.toml`
/// ([`dstack_endpoint::cli::state_path`]).
pub fn state_path(
    windows: bool,
    name: &str,
    local_app_data: Option<OsString>,
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
    temp: PathBuf,
) -> PathBuf {
    dstack_endpoint::cli::state_path(
        windows,
        APP,
        name,
        local_app_data,
        xdg_state_home,
        home,
        temp,
    )
}

/// The dlink device of a run: `name`, its state in `state`, TX only (L and
/// R) at the network's rate, `dstack`/`dsend` as Dante Controller's
/// manufacturer and model, and the budget, ports and process id the
/// options give (dlink's defaults otherwise: the 15 ms budget, no
/// real-time setup).
pub fn link_config(o: &Options, name: &str, state: PathBuf) -> Config {
    let mut c = Config::new(name, state)
        .tx(TX_NAMES)
        .sample_rate(o.rate)
        .product("dstack", APP);
    if let Some(ms) = o.budget_ms {
        c = c.budget_ns(ms.saturating_mul(1_000_000));
    }
    if let Some(p) = o.ports {
        c = c.ports(p);
    }
    if let Some(id) = o.process_id {
        c = c.process_id(id);
    }
    c
}

#[cfg(test)]
mod tests;
