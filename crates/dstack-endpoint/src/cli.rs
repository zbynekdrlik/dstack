// SPDX-License-Identifier: MIT OR Apache-2.0

//! The command line's parts dlisten and dsend share (dstack #164): each app
//! parses its own flags by hand and calls these. Pure: the binaries pass
//! the process's environment in.

use std::ffi::OsString;
use std::net::Ipv4Addr;
use std::path::PathBuf;

use dlink::bridge::{MAX_APP_BUFFER_NS, MIN_APP_BUFFER_NS};
use dlink::{Config, Place, Ports};

/// The longest Dante device name.
pub const NAME_MAX: usize = 31;

/// `text` as a number for `flag`; the error names both.
pub fn number<T: std::str::FromStr>(flag: &str, text: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{flag} needs a number, not {text:?}"))
}

/// `text` as a latency in ms for `flag`, ns: a whole number of ms with up
/// to six decimals (`1`, `0.25`, `12.041666`; dstack #244). The error names
/// both.
pub fn latency_ms(flag: &str, text: &str) -> Result<u64, String> {
    let bad = || format!("{flag} needs ms with up to 6 decimals, not {text:?}");
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || frac.len() > 6 || (text.contains('.') && !digits(frac)) {
        return Err(bad());
    }
    let whole: u64 = whole.parse().map_err(|_| bad())?;
    let frac: u64 = format!("{frac:0<6}").parse().map_err(|_| bad())?;
    whole
        .checked_mul(1_000_000)
        .and_then(|ns| ns.checked_add(frac))
        .ok_or_else(bad)
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

/// The default device name for a host named `host`: `<host><suffix>`
/// (`-dlisten`, `-dsend`), the host's name made a Dante name (A-Z a-z 0-9
/// and '-', others as '-', no '-' at its ends) and cut so the whole is at
/// most [`NAME_MAX`]; `pc` for a name with nothing left.
pub fn default_name(host: &str, suffix: &str) -> String {
    let cleaned: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let room = NAME_MAX.saturating_sub(suffix.len());
    let short: String = cleaned.trim_matches('-').chars().take(room).collect();
    let short = short.trim_end_matches('-');
    let host = if short.is_empty() { "pc" } else { short };
    format!("{host}{suffix}")
}

/// Where the device of app `app` keeps what Dante Controller sets (names,
/// subscriptions, latency) when `--state` is not given: on Windows
/// `<local_app_data>\dstack\<app>\<name>.toml` (`%LOCALAPPDATA%`), else
/// `<xdg_state_home>/dstack/<app>/<name>.toml` or, without it,
/// `<home>/.local/state/...`; the temporary folder when none is set.
pub fn state_path(
    windows: bool,
    app: &str,
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
        .join(app)
        .join(format!("{name}.toml"))
}

/// The flags with a value both apps take ([`parse_shared`]).
pub const SHARED_FLAGS: [&str; 11] = [
    "--output",
    "--name",
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

/// One shared flag's value, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shared {
    /// `--output <text>`: part of the output's name.
    Output(String),
    /// `--name <name>`: the device's Dante name.
    Name(String),
    /// `--rate <hz>`: the network's rate, over 0.
    Rate(u32),
    /// `--network-latency-ms <ms>`: (a), dlink's Dante network latency, ns.
    NetworkLatencyNs(u64),
    /// `--receive-buffer-ms <ms>`: (b), dlink's receive buffer, ns.
    ReceiveBufferNs(u64),
    /// `--app-buffer-ms <ms>`: the bridge FIFO's target, ns (dstack #247),
    /// within `dlink::bridge::MIN_APP_BUFFER_NS` to `MAX_APP_BUFFER_NS`.
    AppBufferNs(u64),
    /// `--ip <addr>` or `--iface <name>`: the Dante NIC.
    Place(Place),
    /// `--ports <arc>,<cmc>,<flows>`.
    Ports(Ports),
    /// `--process-id <n>`.
    ProcessId(u16),
    /// `--state <file>`.
    State(PathBuf),
}

/// `flag`'s `value` when `flag` is one of [`SHARED_FLAGS`] (`None`
/// otherwise); `placed` is whether `--ip` or `--iface` came already (only
/// one, once). An error says what is wrong.
pub fn parse_shared(flag: &str, value: &str, placed: bool) -> Option<Result<Shared, String>> {
    let shared = match flag {
        "--output" if value.is_empty() => Err("--output needs a name".to_string()),
        "--output" => Ok(Shared::Output(value.to_string())),
        "--name" => Ok(Shared::Name(value.to_string())),
        "--rate" => number(flag, value).and_then(|r| match r {
            0 => Err("--rate needs a rate over 0 Hz".to_string()),
            r => Ok(Shared::Rate(r)),
        }),
        "--network-latency-ms" => latency_ms(flag, value).map(Shared::NetworkLatencyNs),
        "--receive-buffer-ms" => latency_ms(flag, value).map(Shared::ReceiveBufferNs),
        "--app-buffer-ms" => latency_ms(flag, value).and_then(|ns| {
            if (MIN_APP_BUFFER_NS..=MAX_APP_BUFFER_NS).contains(&ns) {
                Ok(Shared::AppBufferNs(ns))
            } else {
                Err(format!(
                    "--app-buffer-ms needs {} to {} ms, not {value}",
                    dlink::ms(MIN_APP_BUFFER_NS),
                    dlink::ms(MAX_APP_BUFFER_NS)
                ))
            }
        }),
        "--ip" | "--iface" if placed => Err("give one of --ip and --iface, once".to_string()),
        "--ip" => value
            .parse::<Ipv4Addr>()
            .map(|ip| Shared::Place(Place::Ip(ip)))
            .map_err(|_| format!("--ip needs an IPv4 address, not {value:?}")),
        "--iface" if value.is_empty() => Err("--iface needs a name".to_string()),
        "--iface" => Ok(Shared::Place(Place::Iface(value.to_string()))),
        "--ports" => parse_ports(value).map(Shared::Ports),
        "--process-id" => number(flag, value).map(Shared::ProcessId),
        "--state" if value.is_empty() => Err("--state needs a path".to_string()),
        "--state" => Ok(Shared::State(PathBuf::from(value))),
        _ => return None,
    };
    Some(shared)
}

/// The latency terms the shared options set, ns (dstack #244, #247):
/// `None` keeps the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Latency {
    /// (a), `--network-latency-ms`.
    pub network_ns: Option<u64>,
    /// (b), `--receive-buffer-ms`.
    pub receive_buffer_ns: Option<u64>,
    /// The bridge FIFO's target, `--app-buffer-ms` (dstack #247).
    pub app_buffer_ns: Option<u64>,
}

/// The chunk dsend and dlisten hand the bridge at once, ns (dstack #247):
/// WASAPI's shared-mode period, 10 ms (the engine's default; cpal exposes
/// no period, and dsend's loopback and dlisten's output both run on it).
/// Their app buffer's default is this and one pump block.
pub const WASAPI_PERIOD_NS: u64 = 10_000_000;

/// The device config `c` with the shared options: the latency terms, the
/// ports and the process id when given (dlink's defaults otherwise: (a)
/// 1 ms, (b) keeping v0.6.105's latency, no real-time setup), and the
/// bridge's app buffer: [`WASAPI_PERIOD_NS`] as the chunk, and
/// `--app-buffer-ms` when given (dstack #247).
pub fn with_shared(
    mut c: Config,
    latency: Latency,
    ports: Option<Ports>,
    process_id: Option<u16>,
) -> Config {
    if let Some(ns) = latency.network_ns {
        c = c.network_latency_ns(ns);
    }
    if let Some(ns) = latency.receive_buffer_ns {
        c = c.receive_buffer_ns(ns);
    }
    c = c.app_buffer(WASAPI_PERIOD_NS, latency.app_buffer_ns);
    if let Some(p) = ports {
        c = c.ports(p);
    }
    if let Some(id) = process_id {
        c = c.process_id(id);
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(flag: &str, value: &str) -> Result<Shared, String> {
        parse_shared(flag, value, false).expect("a shared flag")
    }

    #[test]
    fn every_shared_flag_is_read_and_checked() {
        assert_eq!(
            shared("--output", "hdmi"),
            Ok(Shared::Output("hdmi".into()))
        );
        assert_eq!(shared("--output", ""), Err("--output needs a name".into()));
        assert_eq!(shared("--name", "pc-x"), Ok(Shared::Name("pc-x".into())));
        assert_eq!(shared("--rate", "96000"), Ok(Shared::Rate(96_000)));
        assert_eq!(
            shared("--rate", "0"),
            Err("--rate needs a rate over 0 Hz".into())
        );
        assert_eq!(
            shared("--rate", "x"),
            Err("--rate needs a number, not \"x\"".into())
        );
        assert_eq!(
            shared("--network-latency-ms", "0.25"),
            Ok(Shared::NetworkLatencyNs(250_000))
        );
        assert_eq!(
            shared("--receive-buffer-ms", "3"),
            Ok(Shared::ReceiveBufferNs(3_000_000))
        );
        assert_eq!(
            shared("--receive-buffer-ms", "x"),
            Err("--receive-buffer-ms needs ms with up to 6 decimals, not \"x\"".into())
        );
        assert_eq!(
            shared("--app-buffer-ms", "40"),
            Ok(Shared::AppBufferNs(40_000_000))
        );
        assert_eq!(
            shared("--app-buffer-ms", "11.5"),
            Ok(Shared::AppBufferNs(11_500_000))
        );
        assert_eq!(
            shared("--app-buffer-ms", "0.5"),
            Err("--app-buffer-ms needs 1 to 150 ms, not 0.5".into())
        );
        assert_eq!(
            shared("--app-buffer-ms", "151"),
            Err("--app-buffer-ms needs 1 to 150 ms, not 151".into())
        );
        assert_eq!(
            shared("--ip", "192.0.2.7"),
            Ok(Shared::Place(Place::Ip(Ipv4Addr::new(192, 0, 2, 7))))
        );
        assert_eq!(
            shared("--ip", "x"),
            Err("--ip needs an IPv4 address, not \"x\"".into())
        );
        assert_eq!(
            shared("--iface", "eth1"),
            Ok(Shared::Place(Place::Iface("eth1".into())))
        );
        assert_eq!(shared("--iface", ""), Err("--iface needs a name".into()));
        for flag in ["--ip", "--iface"] {
            assert_eq!(
                parse_shared(flag, "eth1", true),
                Some(Err("give one of --ip and --iface, once".into()))
            );
        }
        assert!(matches!(shared("--ports", "1,2,3"), Ok(Shared::Ports(_))));
        assert!(shared("--ports", "1,2").is_err());
        assert_eq!(shared("--process-id", "3"), Ok(Shared::ProcessId(3)));
        assert_eq!(
            shared("--state", "/s.toml"),
            Ok(Shared::State("/s.toml".into()))
        );
        assert_eq!(shared("--state", ""), Err("--state needs a path".into()));
        assert_eq!(parse_shared("--channels", "2", false), None);
        for flag in SHARED_FLAGS {
            assert!(parse_shared(flag, "1", false).is_some(), "{flag}");
        }
    }

    #[test]
    fn the_shared_options_go_into_the_device_config() {
        let bare = || Config::new("n", "/s.toml");
        // The WASAPI chunk always, the configured buffer when given.
        let c = || bare().app_buffer(WASAPI_PERIOD_NS, None);
        assert_eq!(with_shared(bare(), Latency::default(), None, None), c());
        let ports = Ports {
            arc: 1,
            cmc: 2,
            flows: 3,
        };
        let both = Latency {
            network_ns: Some(500_000),
            receive_buffer_ns: Some(3_000_000),
            app_buffer_ns: None,
        };
        assert_eq!(
            with_shared(bare(), both, Some(ports), Some(3)),
            c().network_latency_ns(500_000)
                .receive_buffer_ns(3_000_000)
                .ports(ports)
                .process_id(3)
        );
        // Each term alone.
        let a = Latency {
            network_ns: Some(2_000_000),
            ..Latency::default()
        };
        assert_eq!(
            with_shared(bare(), a, None, None),
            c().network_latency_ns(2_000_000)
        );
        let b = Latency {
            receive_buffer_ns: Some(4_000_000),
            ..Latency::default()
        };
        assert_eq!(
            with_shared(bare(), b, None, None),
            c().receive_buffer_ns(4_000_000)
        );
        let buffered = Latency {
            app_buffer_ns: Some(40_000_000),
            ..Latency::default()
        };
        let config = with_shared(bare(), buffered, None, None);
        assert_eq!(
            config,
            bare().app_buffer(WASAPI_PERIOD_NS, Some(40_000_000))
        );
        assert_eq!(config.bridge_buffer().unwrap().target_ns(), 40_000_000);
    }

    #[test]
    fn a_number_or_the_flag_s_error() {
        assert_eq!(number::<u32>("--rate", "44100"), Ok(44_100));
        assert_eq!(
            number::<u32>("--rate", "x"),
            Err("--rate needs a number, not \"x\"".into())
        );
    }

    /// dstack #244: Dante latencies are fractions of a ms (0.25, 0.5): ms
    /// with up to six decimals, read exactly as ns.
    #[test]
    fn a_latency_is_ms_with_up_to_six_decimals() {
        let f = "--network-latency-ms";
        assert_eq!(latency_ms(f, "1"), Ok(1_000_000));
        assert_eq!(latency_ms(f, "0"), Ok(0));
        assert_eq!(latency_ms(f, "0.25"), Ok(250_000));
        assert_eq!(latency_ms(f, "12.041666"), Ok(12_041_666));
        assert_eq!(latency_ms(f, "2.000001"), Ok(2_000_001));
        assert_eq!(
            latency_ms(f, "18446744073709"),
            Ok(18_446_744_073_709_000_000)
        );
        for bad in [
            "",
            ".",
            "1.",
            ".5",
            "-1",
            "1.1234567",
            "a",
            "1.a",
            "1,5",
            "1.2.3",
            " 1",
            "18446744073710",
            "99999999999999999999",
        ] {
            assert_eq!(
                latency_ms(f, bad),
                Err(format!("{f} needs ms with up to 6 decimals, not {bad:?}")),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn ports_are_three_ports_over_zero() {
        assert_eq!(
            parse_ports("1, 2,3"),
            Ok(Ports {
                arc: 1,
                cmc: 2,
                flows: 3
            })
        );
        for bad in ["1,2", "1,2,3,4", "0,2,3", "a,2,3", ""] {
            assert_eq!(
                parse_ports(bad),
                Err(format!("--ports is <arc>,<cmc>,<flows>, not {bad:?}"))
            );
        }
    }

    #[test]
    fn the_default_name_takes_the_app_s_suffix_within_31() {
        assert_eq!(default_name("FOH-PC", "-dsend"), "FOH-PC-dsend");
        assert_eq!(default_name("a.b", "-dlisten"), "a-b-dlisten");
        assert_eq!(default_name("", "-dsend"), "pc-dsend");
        let long = default_name("abcdefghijklmnopqrstuvwxyz0123", "-dsend");
        assert_eq!(long, "abcdefghijklmnopqrstuvwxy-dsend");
        assert_eq!(long.len(), NAME_MAX);
        // A suffix longer than a name leaves no room, never a panic.
        assert_eq!(
            default_name("pc", &"x".repeat(40)),
            format!("pc{}", "x".repeat(40))
        );
    }

    #[test]
    fn the_state_file_is_under_the_app_s_folder() {
        let os = |s: &str| Some(OsString::from(s));
        assert_eq!(
            state_path(
                true,
                "dsend",
                "pc-dsend",
                os("C:\\L"),
                None,
                None,
                "/t".into()
            ),
            PathBuf::from("C:\\L")
                .join("dstack")
                .join("dsend")
                .join("pc-dsend.toml")
        );
        assert_eq!(
            state_path(
                false,
                "dsend",
                "pc-dsend",
                None,
                None,
                os("/h"),
                "/t".into()
            ),
            PathBuf::from("/h/.local/state/dstack/dsend/pc-dsend.toml")
        );
    }
}
