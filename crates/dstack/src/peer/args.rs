// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! `dstack-dev peer`'s command line (dstack #305; [`USAGE`]).

use std::net::Ipv4Addr;
use std::path::PathBuf;

use crate::clock::TestClockPpm;
use crate::daemon::Place;

/// `dstack-dev peer --help`.
pub const USAGE: &str = "\
usage: dstack-dev peer (--iface <if> | --ip <ipv4>) --name <device> --status <path>
         [--tx <n>] [--tone-hz <hz,...>] [--rx-from <device>:<channel,...>]
         [--test-clock-ppm <ppm>] [--rate <hz>] [--latency-ms <ms>]
         [--ports <arc>,<cmc>,<flows>] [--process-id <n>] [--state <path>]

A TEST Dante device on the system clock (no PTP network: CI), dstack #305.
  --tx <n>              TX channels \"TX 1\".. (default: one per --tone-hz)
  --tone-hz <hz,...>    the TX channels' sines, whole Hz, -6 dBFS: one each,
                        or one for every channel (200 Hz or over keeps the
                        RX check's margin)
  --rx-from <dev>:<ch>  RX channels \"RX 1\".., subscribed to these TX
                        channels of <dev> (names as Dante lists them)
  --test-clock-ppm <p>  the media clock runs <p> ppm off this machine's
                        clock (-1000 to 1000; TEST ONLY)
  --rate <hz>           sample rate (48000)
  --latency-ms <ms>     the device latency (10)
  --ports a,c,f         ARC, CMC and flow-control ports (4440,8800,4455)
  --process-id <n>      the process id (0): another device on this address
                        needs its own ports and id
  --state <path>        the state file (<status>.state.toml)
  --status <path>       a JSON status, rewritten once a second
Stops on SIGINT or SIGTERM (exit 0 after the goodbyes).";

/// What the peer runs.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerArgs {
    pub place: Place,
    pub name: String,
    /// Each TX channel's tone, Hz (one per channel).
    pub tones: Vec<u32>,
    /// The device and its TX channels the RX channels take, in order.
    pub rx_from: Option<(String, Vec<String>)>,
    pub test_clock_ppm: Option<TestClockPpm>,
    pub rate: u32,
    pub latency_ns: u64,
    /// ARC, CMC and flow-control ports.
    pub ports: Option<(u16, u16, u16)>,
    pub process_id: u16,
    pub state: PathBuf,
    pub status: PathBuf,
}

/// A parsed command line: run, or print the usage.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Run(Box<PeerArgs>),
    Help,
}

/// Parses `args` (after `peer`); every refusal names its flag.
pub fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut place = None;
    let mut name = None;
    let mut tx = None;
    let mut tones = Vec::new();
    let mut rx_from = None;
    let mut test_clock_ppm = None;
    let mut rate = 48_000;
    let mut latency_ns = 10_000_000;
    let mut ports = None;
    let mut process_id = 0;
    let mut state = None;
    let mut status = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        if flag == "--help" || flag == "-h" {
            return Ok(Parsed::Help);
        }
        let value = it
            .next()
            .ok_or_else(|| format!("{flag}: a value is missing"))?;
        let bad = |what: &str| format!("{flag} {value}: {what}");
        match flag.as_str() {
            "--iface" => place = Some(Place::Iface(value.clone())),
            "--ip" => {
                place = Some(Place::Ip(
                    value
                        .parse::<Ipv4Addr>()
                        .map_err(|_| bad("an IPv4 address"))?,
                ));
            }
            "--name" => name = Some(value.clone()),
            "--tx" => tx = Some(value.parse::<usize>().map_err(|_| bad("a count"))?),
            "--tone-hz" => {
                tones = list(value)
                    .map(|v| v.parse::<u32>().ok().filter(|&hz| hz > 0))
                    .collect::<Option<Vec<u32>>>()
                    .ok_or_else(|| bad("whole Hz, comma separated"))?;
            }
            "--rx-from" => {
                let (device, channels) = value
                    .split_once(':')
                    .ok_or_else(|| bad("<device>:<channel,...>"))?;
                let channels: Vec<String> = list(channels).map(str::to_string).collect();
                if device.is_empty() || channels.is_empty() {
                    return Err(bad("<device>:<channel,...>"));
                }
                rx_from = Some((device.to_string(), channels));
            }
            "--test-clock-ppm" => {
                let ppm = TestClockPpm(value.parse::<f64>().map_err(|_| bad("a number"))?);
                if let Some(problem) = ppm.problem() {
                    return Err(problem);
                }
                test_clock_ppm = Some(ppm);
            }
            "--rate" => rate = value.parse::<u32>().map_err(|_| bad("Hz"))?,
            "--latency-ms" => {
                let ms = value
                    .parse::<f64>()
                    .ok()
                    .filter(|ms| *ms > 0.0 && *ms <= 1000.0)
                    .ok_or_else(|| bad("ms, over 0 and up to 1000"))?;
                latency_ns = (ms * 1e6).round() as u64;
            }
            "--ports" => {
                let p = list(value)
                    .map(|v| v.parse::<u16>().ok())
                    .collect::<Option<Vec<u16>>>()
                    .filter(|p| p.len() == 3)
                    .ok_or_else(|| bad("<arc>,<cmc>,<flows>"))?;
                ports = Some((p[0], p[1], p[2]));
            }
            "--process-id" => process_id = value.parse::<u16>().map_err(|_| bad("a number"))?,
            "--state" => state = Some(PathBuf::from(value)),
            "--status" => status = Some(PathBuf::from(value)),
            _ => return Err(format!("{flag}: not a peer option")),
        }
    }
    let place = place.ok_or("--iface or --ip is missing")?;
    let name = name.ok_or("--name is missing")?;
    let status = status.ok_or("--status is missing")?;
    // `--tx n` with one tone: every channel sends it; with several: one
    // a channel.
    match (tx, tones.len()) {
        (Some(n), 1) => tones = vec![tones[0]; n],
        (Some(n), len) if n != len => {
            return Err(format!(
                "--tx {n}: --tone-hz gives {len} tones: one for every channel, or one each"
            ));
        }
        _ => {}
    }
    if tones.iter().any(|&hz| u64::from(hz) * 2 >= u64::from(rate)) {
        return Err(format!("--tone-hz: each under half the rate ({rate} Hz)"));
    }
    let state = state.unwrap_or_else(|| {
        let mut s = status.clone().into_os_string();
        s.push(".state.toml");
        PathBuf::from(s)
    });
    Ok(Parsed::Run(Box::new(PeerArgs {
        place,
        name,
        tones,
        rx_from,
        test_clock_ppm,
        rate,
        latency_ns,
        ports,
        process_id,
        state,
        status,
    })))
}

/// A comma separated list's items, trimmed.
fn list(value: &str) -> impl Iterator<Item = &str> {
    value.split(',').map(str::trim).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests;
