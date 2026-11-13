// SPDX-License-Identifier: MIT OR Apache-2.0

//! dlisten's command line (dstack #139), parsed by hand as `dstack-dev`'s
//! is: a handful of flags need no parser crate. Pure: the binary reads the
//! process's arguments and environment and passes them in.

use std::ffi::OsString;
use std::path::PathBuf;

use dlink::{Config, Place, Ports};
use dstack_endpoint::cli::{Latency, Shared, number, parse_shared, with_shared};

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
  --mono                      every RX channel summed at 0 dB to both
                              speakers (clamped at full scale)
  --name <name>               the device's Dante name (default <host>-dlisten)
  --subscribe <rx>=<ch>@<dev> our RX channel <rx> (from 1) receives TX
                              channel <ch> of device <dev>; repeatable. Only
                              our own device is changed. A subscription set
                              in Dante Controller wins.
  --rate <hz>                 the network's sample rate (default 48000)
  --network-latency-ms <ms>   (a) the Dante network latency (default 1)
  --receive-buffer-ms <ms>    (b) the receive buffer; the Dante latency is
                              (a) + (b) (default: as before 0.6.106)
  --app-buffer-ms <ms>        the buffer between the network and the
                              output, 1 to 150 (default: a 10 ms output
                              period, WASAPI's, assumed, + one pump block,
                              12.67 at 48000 Hz; was 40 before 0.6.112);
                              a longer period (ALSA) needs at least half
                              of it + a block
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
pub use dstack_endpoint::cli::{NAME_MAX, parse_ports};

/// The flags that take a value.
const VALUE_FLAGS: [&str; 13] = [
    "--output",
    "--channels",
    "--name",
    "--subscribe",
    "--rate",
    "--network-latency-ms",
    "--receive-buffer-ms",
    "--app-buffer-ms",
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
    /// The latency terms (a) and (b) and the app buffer, ns (dstack #244,
    /// #247).
    pub latency: Latency,
    /// None: the default route's address.
    pub place: Option<Place>,
    pub ports: Option<Ports>,
    pub process_id: Option<u16>,
    pub state: Option<PathBuf>,
    /// `--mono`: every RX channel summed at 0 dB to both speakers.
    pub mono: bool,
}

impl Options {
    /// Takes one shared flag's value ([`parse_shared`]).
    fn set(&mut self, v: Shared) {
        match v {
            Shared::Output(t) => self.output = Some(t),
            Shared::Name(n) => self.name = Some(n),
            Shared::Rate(r) => self.rate = r,
            Shared::NetworkLatencyNs(ns) => self.latency.network_ns = Some(ns),
            Shared::ReceiveBufferNs(ns) => self.latency.receive_buffer_ns = Some(ns),
            Shared::AppBufferNs(ns) => self.latency.app_buffer_ns = Some(ns),
            Shared::Place(p) => self.place = Some(p),
            Shared::Ports(p) => self.ports = Some(p),
            Shared::ProcessId(id) => self.process_id = Some(id),
            Shared::State(f) => self.state = Some(f),
        }
    }
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
        latency: Latency::default(),
        place: None,
        ports: None,
        process_id: None,
        state: None,
        mono: false,
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
            "--mono" => {
                o.mono = true;
                continue;
            }
            _ => {}
        }
        if !VALUE_FLAGS.contains(&flag) {
            return Err(format!("unknown argument {flag:?}"));
        }
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag {
            "--channels" => {
                let n: usize = number(flag, value)?;
                if !(1..=MAX_CHANNELS).contains(&n) {
                    return Err(format!("--channels is 1 to {MAX_CHANNELS}, not {n}"));
                }
                o.channels = n;
            }
            "--subscribe" => o.subscribe.push(parse_subscribe(value)?),
            _ => match parse_shared(flag, value, o.place.is_some()) {
                Some(v) => o.set(v?),
                None => return Err(format!("unknown argument {flag:?}")),
            },
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
    dstack_endpoint::cli::default_name(host, SUFFIX)
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
    dstack_endpoint::cli::state_path(
        windows,
        "dlisten",
        name,
        local_app_data,
        xdg_state_home,
        home,
        temp,
    )
}

/// The dlink device of a run: `name`, its state in `state`, RX only with
/// the options' channels at the network's rate, `dstack`/`dlisten` as
/// Dante Controller's manufacturer and model, and the latency terms,
/// ports, process id and subscriptions the options give (dlink's
/// defaults otherwise: (a) 1 ms, (b) keeping v0.6.105's latency, no
/// real-time setup).
pub fn link_config(o: &Options, name: &str, state: PathBuf) -> Config {
    let mut c = Config::new(name, state)
        .rx(rx_names(o.channels))
        .sample_rate(o.rate)
        .product("dstack", "dlisten");
    c = with_shared(c, o.latency, o.ports, o.process_id);
    for s in &o.subscribe {
        c = c.subscribe(s.rx, s.tx_channel.clone(), s.tx_device.clone());
    }
    c
}

#[cfg(test)]
mod tests;
