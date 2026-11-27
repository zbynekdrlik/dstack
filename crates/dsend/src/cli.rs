// SPDX-License-Identifier: MIT OR Apache-2.0

//! dsend's command line (dstack #164), parsed by hand as dlisten's is. Pure:
//! the binary reads the process's arguments and environment and passes
//! them in.

use std::ffi::OsString;
use std::path::PathBuf;

use dlink::{Config, Place, Ports};
use dstack_endpoint::cli::{Latency, SHARED_FLAGS, Shared, latency_ms, parse_shared, with_shared};

use crate::source::TX_NAMES;

/// The usage text (`--help`, and after a usage error).
pub const USAGE: &str = "\
usage: dsend [options]
Sends this PC's sound into Dante: an output's loopback (what plays on it)
as the TX channels L and R of a Dante device <host>-dsend, converted to
the network's rate and clock. Windows only (WASAPI loopback).

  --list-outputs              list the outputs and exit
  --output <text>             capture the first output whose name holds
                              <text> (any case; default: the saved choice,
                              else dsend's own output \"dsend\" when its
                              driver is installed, else the default output)
  --name <name>               the device's Dante name (default <host>-dsend)
  --rate <hz>                 the network's sample rate (default 96000)
  --latency-ms <ms>           the Dante latency, (a) + (b) (default 10,
                              the most hardware receivers accept: past a
                              receiver's maximum it shows \"Incorrect
                              channel latencies\"; 40 in 0.6.202-0.6.211,
                              9 before)
  --network-latency-ms <ms>   (a) the Dante network latency (default 1)
  --receive-buffer-ms <ms>    (b) the receive buffer, instead of
                              --latency-ms; the Dante latency is (a) + (b)
                              (default: --latency-ms less (a))
  --app-buffer-ms <ms>        the buffer between the captured output and
                              the network, 1 to 150 (default: WASAPI's
                              10 ms period, assumed, + one pump block,
                              11.33 at 96000 Hz; was 40 before 0.6.112);
                              under half the period + a block drops out
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
The status is also logged every 10 s, to stderr and to <state file>.log;
a start, and a log past 10 MiB, moves it to <state file>.log.1 (the older
ones up to .log.3). Ctrl+C stops it.";

/// The network's rate unless `--rate`.
pub const RATE: u32 = 96_000;
/// The name's suffix after the host's name.
pub const SUFFIX: &str = "-dsend";
/// The app's name (its state folder, the command's usage).
pub const APP: &str = "dsend";

/// The shared flags that take a value; dsend's own [`LATENCY_FLAG`] is read
/// before them.
const VALUE_FLAGS: [&str; 11] = SHARED_FLAGS;

/// dsend's own flag (dstack #480): the Dante latency P, ms.
pub const LATENCY_FLAG: &str = "--latency-ms";

/// What a run is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub name: Option<String>,
    pub output: Option<String>,
    pub rate: u32,
    /// The latency terms (a) and (b) and the app buffer, ns (dstack #244,
    /// #247).
    pub latency: Latency,
    /// The Dante latency P, ns, `--latency-ms` (dstack #480): None keeps
    /// [`LATENCY_NS`]. Never given with (b).
    pub latency_ns: Option<u64>,
    /// None: the default route's address.
    pub place: Option<Place>,
    pub ports: Option<Ports>,
    pub process_id: Option<u16>,
    pub state: Option<PathBuf>,
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
    Help,
    Version,
}

/// The command line after the program's name.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut o = Options {
        name: None,
        output: None,
        rate: RATE,
        latency: Latency::default(),
        latency_ns: None,
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
        if flag == LATENCY_FLAG {
            let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
            o.latency_ns = Some(latency_ms(flag, value)?);
            continue;
        }
        if !VALUE_FLAGS.contains(&flag) {
            return Err(format!("unknown argument {flag:?}"));
        }
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match parse_shared(flag, value, o.place.is_some()) {
            Some(v) => o.set(v?),
            None => return Err(format!("unknown argument {flag:?}")),
        }
    }
    latency_check(o.latency, o.latency_ns)?;
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

/// Whether a failure shows a message box (dstack #225): never for a run
/// (`run`) whose log (`<state file>.log`) is open (`logged`), which has no
/// window and is started headless (a logon task: no console, no stderr),
/// so its failure goes to stderr and that log only; otherwise (another
/// command, or a run without its log, which would leave no trace) as
/// [`dstack_endpoint::alert::shows_box`] says (a start from Explorer or a
/// task).
pub fn shows_box(run: bool, logged: bool, attached: bool, has_stderr: bool) -> bool {
    !(run && logged) && dstack_endpoint::alert::shows_box(attached, has_stderr)
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

/// dsend's Dante latency unless `--latency-ms` or `--receive-buffer-ms`
/// sets it, ns: 10 ms, the largest value common hardware receivers accept
/// (dstack #480). A Dante receiver plays a flow at the larger of its own
/// latency and the one the transmitter advertises (`latency_ns=` in our
/// channel records, before any subscription; the flow request carries no
/// latency), and only within its own range: the site's hardware states a
/// maximum of 10.156 ms (a card, the wireless receiver) and 10.667 ms
/// (another card) in its ARC settings. #461's 40 ms (owner, 20.11.2026:
/// dsend is a fallback on the FOH PC and never raises its threads'
/// priority, so it rides the PC's CPU holds out in its latency, as far as
/// the receiver allows) was past them: the FOH card's subscription read
/// "Incorrect channel latencies". 10 ms keeps the most margin hardware
/// takes (dlink's default gave dsend 9 ms, dstack #247); a site whose
/// receivers accept more sets `--latency-ms`.
pub const LATENCY_NS: u64 = 10_000_000;

/// The latency terms dsend's device runs at (dstack #461, #480): those
/// `given`, and with no (b) given, (b) = `total` (`--latency-ms`, else
/// [`LATENCY_NS`]) less (a) (`given`'s, else dlink's 1 ms), so the Dante
/// latency is that total. [`parse`] refuses (b) with a total, and a total
/// under (a); here (b) never wraps below 0.
pub fn latency_of(given: Latency, total: Option<u64>) -> Latency {
    let network_ns = network_of(given);
    let total_ns = total.unwrap_or(LATENCY_NS);
    Latency {
        receive_buffer_ns: given
            .receive_buffer_ns
            .or(Some(total_ns.saturating_sub(network_ns))),
        ..given
    }
}

/// The network latency (a) of `given`, ns: as set, else dlink's 1 ms.
fn network_of(given: Latency) -> u64 {
    given.network_ns.unwrap_or(dlink::NETWORK_LATENCY_NS)
}

/// Whether the latency options agree (dstack #480): `--latency-ms` sets the
/// whole latency, so it never comes with `--receive-buffer-ms` (b), and it
/// is never under the network latency (a) it holds (`given`'s, else
/// dlink's 1 ms). The error says which.
pub fn latency_check(given: Latency, total: Option<u64>) -> Result<(), String> {
    let Some(total) = total else {
        return Ok(());
    };
    if given.receive_buffer_ns.is_some() {
        return Err(format!(
            "give one of {LATENCY_FLAG} (the whole latency) and --receive-buffer-ms (b)"
        ));
    }
    let network_ns = network_of(given);
    if total < network_ns {
        return Err(format!(
            "{LATENCY_FLAG} {} is under the network latency (a) {} ms it holds",
            dlink::ms(total),
            dlink::ms(network_ns)
        ));
    }
    Ok(())
}

/// The dlink device of a run: `name`, its state in `state`, TX only (L and
/// R) at the network's rate, `dstack`/`dsend` as Dante Controller's
/// manufacturer and model, and the latency terms ([`latency_of`]: a Dante
/// latency of 10 ms unless set), ports and process id the options give (no
/// real-time setup).
pub fn link_config(o: &Options, name: &str, state: PathBuf) -> Config {
    let mut c = Config::new(name, state)
        .tx(TX_NAMES)
        .sample_rate(o.rate)
        .product("dstack", APP);
    c = with_shared(
        c,
        latency_of(o.latency, o.latency_ns),
        o.ports,
        o.process_id,
    );
    c
}

#[cfg(test)]
mod tests;
