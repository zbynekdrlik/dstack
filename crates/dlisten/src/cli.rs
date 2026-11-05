// SPDX-License-Identifier: MIT OR Apache-2.0

//! dlisten's command line (dstack #139), parsed by hand as `dstack-dev`'s
//! is: a handful of flags need no parser crate. Pure: the binary reads the
//! process's arguments and environment and passes them in.

use std::ffi::OsString;
use std::net::Ipv4Addr;
use std::path::PathBuf;

use dlink::{Config, Place, Ports};

/// The usage text (`--help`, and after a usage error).
pub const USAGE: &str = "\
usage: dlisten [options]
Plays Dante audio on a local output: a Dante device <host>-dlisten that
receives, played on the output at its own rate and clock.

  --list-outputs              list the outputs and exit
  --autostart on|off          start dlisten at the user's logon (with the
                              other options given here) or not, and exit
  --output <text>             the first output whose name holds <text>
                              (any case; default: the default output)
  --channels <n>              the device's RX channels (default 2, L and R;
                              1 to 64)
  --name <name>               the device's Dante name (default <host>-dlisten)
  --subscribe <rx>=<ch>@<dev> our RX channel <rx> (from 1) receives TX
                              channel <ch> of device <dev>; repeatable. Only
                              our own device is changed. A subscription set
                              in Dante Controller wins.
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

dlisten has no window: dtray shows it (the status, and an Output menu
whose choice is remembered for the next start; --output wins). The status
is also logged every 10 s, to stderr and to <state file>.log. Ctrl+C
stops it.";

/// The RX channels unless `--channels`.
pub const CHANNELS: usize = 2;
/// The most RX channels.
pub const MAX_CHANNELS: usize = 64;
/// The network's rate unless `--rate`.
pub const RATE: u32 = 48_000;
/// The name's suffix after the host's name.
pub const SUFFIX: &str = "-dlisten";
/// The longest Dante device name.
pub const NAME_MAX: usize = 31;

/// The flags that take a value.
const VALUE_FLAGS: [&str; 11] = [
    "--output",
    "--channels",
    "--name",
    "--subscribe",
    "--rate",
    "--budget-ms",
    "--ip",
    "--iface",
    "--ports",
    "--process-id",
    "--state",
];

/// One `--subscribe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscribe {
    /// Our RX channel, from 1.
    pub rx: usize,
    pub tx_channel: String,
    pub tx_device: String,
}

/// What a run is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub name: Option<String>,
    pub channels: usize,
    pub output: Option<String>,
    pub subscribe: Vec<Subscribe>,
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
    /// Start at logon (`on`) with these arguments, or not.
    Autostart {
        on: bool,
        args: Vec<String>,
    },
    Help,
    Version,
}

fn number<T: std::str::FromStr>(flag: &str, text: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{flag} needs a number, not {text:?}"))
}

/// `<rx>=<channel>@<device>`: the channel may hold `@` (the device's name
/// cannot: Dante names are A-Z a-z 0-9 and '-'), so the last `@` splits.
pub fn parse_subscribe(text: &str) -> Result<Subscribe, String> {
    let bad = || format!("--subscribe is <rx>=<channel>@<device>, not {text:?}");
    let (rx, rest) = text.split_once('=').ok_or_else(bad)?;
    let (channel, device) = rest.rsplit_once('@').ok_or_else(bad)?;
    let rx: usize = rx.trim().parse().map_err(|_| bad())?;
    if rx == 0 || channel.is_empty() || device.is_empty() {
        return Err(bad());
    }
    Ok(Subscribe {
        rx,
        tx_channel: channel.to_string(),
        tx_device: device.to_string(),
    })
}

/// `<arc>,<cmc>,<flows>`, each a port over 0.
pub fn parse_ports(text: &str) -> Result<Ports, String> {
    let bad = || format!("--ports is <arc>,<cmc>,<flows>, not {text:?}");
    let parts: Vec<u16> = text
        .split(',')
        .map(|p| p.trim().parse::<u16>().ok().filter(|&p| p > 0))
        .collect::<Option<_>>()
        .ok_or_else(bad)?;
    match parts[..] {
        [arc, cmc, flows] => Ok(Ports { arc, cmc, flows }),
        _ => Err(bad()),
    }
}

/// The command line after the program's name.
pub fn parse(args: &[String]) -> Result<Command, String> {
    if let Some(at) = args.iter().position(|a| a == "--autostart") {
        let on = match args.get(at + 1).map(String::as_str) {
            Some("on") => true,
            Some("off") => false,
            _ => return Err("--autostart is on or off".into()),
        };
        let rest: Vec<String> = args[..at].iter().chain(&args[at + 2..]).cloned().collect();
        // The options the logon start will run with are checked now.
        return match parse(&rest)? {
            Command::Run(_) => Ok(Command::Autostart { on, args: rest }),
            _ => Err("--autostart takes run options only".into()),
        };
    }
    let mut o = Options {
        name: None,
        channels: CHANNELS,
        output: None,
        subscribe: Vec::new(),
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
            "--channels" => {
                let n: usize = number(flag, value)?;
                if !(1..=MAX_CHANNELS).contains(&n) {
                    return Err(format!("--channels is 1 to {MAX_CHANNELS}, not {n}"));
                }
                o.channels = n;
            }
            "--name" => o.name = Some(value.clone()),
            "--subscribe" => o.subscribe.push(parse_subscribe(value)?),
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
    if let Some(s) = o.subscribe.iter().find(|s| s.rx > o.channels) {
        return Err(format!(
            "--subscribe {}: the device has {} RX channels",
            s.rx, o.channels
        ));
    }
    Ok(if list {
        Command::ListOutputs
    } else {
        Command::Run(o)
    })
}

/// The default device name for a host named `host`: `<host>-dlisten`, the
/// host's name made a Dante name (A-Z a-z 0-9 and '-', others as '-', no
/// '-' at its ends) and cut so the whole is at most [`NAME_MAX`]; `pc` for
/// a name with nothing left.
pub fn default_name(host: &str) -> String {
    let cleaned: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let room = NAME_MAX - SUFFIX.len();
    let short: String = cleaned.trim_matches('-').chars().take(room).collect();
    let short = short.trim_end_matches('-');
    let host = if short.is_empty() { "pc" } else { short };
    format!("{host}{SUFFIX}")
}

/// The RX channels' names: L and R for two, else `In 1` to `In <n>`.
pub fn rx_names(n: usize) -> Vec<String> {
    if n == 2 {
        vec!["L".into(), "R".into()]
    } else {
        (1..=n).map(|i| format!("In {i}")).collect()
    }
}

/// Where the device keeps what Dante Controller sets (names,
/// subscriptions, latency) when `--state` is not given: on Windows
/// `<local_app_data>\dstack\dlisten\<name>.toml` (`%LOCALAPPDATA%`), else
/// `<xdg_state_home>/dstack/dlisten/<name>.toml` or, without it,
/// `<home>/.local/state/...`; the temporary folder when none is set.
pub fn state_path(
    windows: bool,
    name: &str,
    local_app_data: Option<OsString>,
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
    temp: PathBuf,
) -> PathBuf {
    let set = |v: Option<OsString>| v.filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if windows {
        set(local_app_data)
    } else {
        set(xdg_state_home).or_else(|| set(home).map(|h| h.join(".local").join("state")))
    };
    base.unwrap_or(temp)
        .join("dstack")
        .join("dlisten")
        .join(format!("{name}.toml"))
}

/// The dlink device of a run: `name`, its state in `state`, RX only with
/// the options' channels at the network's rate, `dstack`/`dlisten` as
/// Dante Controller's manufacturer and model, and the budget, ports,
/// process id and subscriptions the options give (dlink's defaults
/// otherwise: the 15 ms budget, no real-time setup).
pub fn link_config(o: &Options, name: &str, state: PathBuf) -> Config {
    let mut c = Config::new(name, state)
        .rx(rx_names(o.channels))
        .sample_rate(o.rate)
        .product("dstack", "dlisten");
    if let Some(ms) = o.budget_ms {
        c = c.budget_ns(ms.saturating_mul(1_000_000));
    }
    if let Some(p) = o.ports {
        c = c.ports(p);
    }
    if let Some(id) = o.process_id {
        c = c.process_id(id);
    }
    for s in &o.subscribe {
        c = c.subscribe(s.rx, s.tx_channel.clone(), s.tx_device.clone());
    }
    c
}

#[cfg(test)]
mod tests;
