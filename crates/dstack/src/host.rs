// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The device as a library (Gen 3 phase C, plan
//! `docs/superpowers/plans/2026-10-26-gen3-phase-c-engine-on-linux.md`
//! Task 2): what `iem-dante-dev` ran in its `main`, so the engine can host
//! the device in its own process (Task 3) and the standalone binary stays a
//! thin caller. Linux and Windows (dasio plan Task 2: on Windows no
//! control socket, a console Ctrl+C for the signals, the NIC's facts from
//! `GetAdaptersAddresses`).
//!
//! - [`prepare`]: the device's setup, nothing served yet: the config and
//!   its state file (a folder it cannot write is found here), the interface
//!   and its address (waited for while it is not there, dstack #225), its
//!   facts for the network info, the start line, the runtime, the four
//!   servers' sockets, the media side ([`Media`], with an
//!   engine ring on each TX channel the engine feeds) and the control
//!   socket bound. [`Prepared::media`] and [`Prepared::config`] are what the
//!   engine builds its pump from.
//! - [`Prepared::start`]: the mDNS daemon on the interface only, the
//!   signals ([`Signals`]), and the `dstack-device` thread: a current-thread
//!   tokio runtime running the media clock's poller and
//!   [`daemon::serve`] until it is stopped, then the mDNS goodbyes.
//! - [`Host`]: the running device: [`Host::ended`] (serving ended by
//!   itself, and why), [`Host::stop`] (asks serving to end and waits a
//!   bounded time; one still running is a warning, never killed),
//!   [`Host::wait`] (the standalone binary's wait for a signal's end).
//!
//! [`prepare`] and [`Prepared::start`] use the device's own runtime on the
//! caller's thread (its sockets are bound and its signals registered in
//! it): call them, and drop a [`Prepared`] that is never started, outside
//! any tokio runtime.
//!
//! Errors keep the binary's exit codes ([`exit_code`]): 2 for the command
//! line, the config, the state file (also one whose folder cannot be
//! written) or an interface without a usable MAC, which the unit does not
//! restart (`RestartPreventExitStatus=2`); 1 for an address the Dante
//! network cannot reach (an address not there yet is waited for), an
//! interface with several IPv4 addresses, a MAC file that cannot be read,
//! a socket not bound, serving that failed or goodbyes not confirmed,
//! which it restarts. Logs name counts,
//! ports and channel numbers, never a name or an address.

use std::fmt::Display;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mdns_sd::{DaemonStatus, IfKind, ServiceDaemon};
use tokio::net::UdpSocket;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::runtime::Runtime;
#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::announce::Announcer;
use crate::clock::linked::Linked;
use crate::clock::{ClockStats, MonoClock, monotonic_ns, poll_dsync, realtime_ns};
#[cfg(unix)]
use crate::ctl;
use crate::daemon::media::{Clock, Media};
use crate::daemon::{self, BUILD, IfAddr, Options, Place, PlaceWait, ServeError, Sockets, VERSION};
use crate::device::{Device, DeviceConfig, DeviceState};
use crate::info::{self, Multicast, NetInfo};

/// How long the mDNS daemon gets to send the goodbyes and stop.
pub const GOODBYE_WAIT: Duration = Duration::from_secs(3);

/// Why the device could not run, or ended badly; its exit code is
/// [`exit_code`]'s.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// The config, the state file or the interface's MAC: exit 2 (not
    /// restarted).
    #[error("{0}")]
    Setup(String),
    /// The address, a socket, serving or the goodbyes: exit 1 (restarted).
    #[error("{0}")]
    Run(String),
}

/// The exit code of `e`: 2 for [`HostError::Setup`], 1 for
/// [`HostError::Run`] (phase B's).
pub fn exit_code(e: &HostError) -> u8 {
    match e {
        HostError::Setup(_) => 2,
        HostError::Run(_) => 1,
    }
}

fn setup(e: impl Display) -> HostError {
    HostError::Setup(e.to_string())
}

fn run_failed(e: impl Display) -> HostError {
    HostError::Run(e.to_string())
}

/// Where the media clock's PTP time comes from. The default is
/// [`ClockSource::Linked`] for every device of ours (owner 4.11.2026): it
/// follows Dante PTP in its own process, as DVS does, so its audio does not
/// hang on the PC's dsync service; the service serves the rest of the
/// system, on the same grandmaster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClockSource {
    /// dsync on the box, polled once a second: the network's PTP time, and
    /// nothing else (no in-process follower).
    Dsync,
    /// The default (owner 2.11 and 4.11.2026): dsync's in-process
    /// follower (dsync-ptp) on the device's interface when it is locked,
    /// the dsync service otherwise ([`crate::clock::linked`]): a clock with
    /// or without the service, never worse than it.
    #[default]
    Linked,
    /// TEST ONLY: this machine's own clock (realtime, read once, then
    /// monotonic), ready at once, never re-anchored. For a device with no
    /// PTP network (CI): its flows are on no network's time, so another
    /// device cannot play them in sync. Logged as a warning at start.
    System,
}

/// What SIGTERM and SIGINT do while the device serves.
#[derive(Debug, Clone)]
pub enum Signals {
    /// End serving (the standalone device: the unit's stop).
    Stop,
    /// Only set the flag: the host (the engine) decides when to stop,
    /// through [`Host::stop`].
    Report(Arc<AtomicBool>),
    /// No handler at all: an app that hosts the device in its own process
    /// (an OBS plugin) owns its signals and stops the device through
    /// [`Host::stop`]. A registered handler would outlive the device and
    /// keep SIGTERM from ending the app ([`crate::embed`]).
    Host,
}

/// The device's config read from `path`: a file that cannot be read or a
/// config that does not hold is [`HostError::Setup`] naming the path (the
/// config's own errors name a line, never a value).
pub fn read_config(path: &Path) -> Result<DeviceConfig, HostError> {
    let shown = path.display();
    let text = std::fs::read_to_string(path).map_err(|e| setup(format!("{shown}: {e}")))?;
    DeviceConfig::from_toml(&text).map_err(|e| setup(format!("{shown}: {e}")))
}

/// The device of `cfg` with its state file loaded, whose folder must be
/// writable now (found at the start, not at the first rename's save, which
/// would be refused; checked before the load, which saves a state it
/// fitted to the config's channel counts): [`HostError::Setup`] otherwise.
pub fn load_device(cfg: DeviceConfig) -> Result<Device, HostError> {
    DeviceState::check_writable(&cfg.state_file).map_err(|e| {
        setup(format!(
            "the state file cannot be written ({e}): a rename could not be saved; \
             the folder must be writable by the service's user (StateDirectory=iem-dante)"
        ))
    })?;
    Device::load(cfg).map_err(setup)
}

/// The watch that keeps a socket's multicast group joined across an
/// interface change (dstack #282): see [`crate::membership`].
pub use crate::membership::{Link, Membership, links};

/// The box's IPv4 addresses, by interface.
pub fn interfaces() -> std::io::Result<Vec<IfAddr>> {
    Ok(if_addrs::get_if_addrs()?
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some(IfAddr {
                name: i.name,
                ip: v4.ip,
                netmask: v4.netmask,
            }),
            if_addrs::IfAddr::V6(_) => None,
        })
        .collect())
}

/// The default route's address, looked up once (`None` while the box has
/// no default route yet, or its network is still down or unreachable): a
/// UDP socket connected to a TEST-NET-1 address picks the route and sends
/// nothing. An IPv6 route and a socket that cannot be opened are errors.
pub fn default_route() -> Result<Option<Ipv4Addr>, String> {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0")
        .map_err(|e| format!("the default route's probe socket: {e}"))?;
    daemon::route_answer(
        probe
            .connect("192.0.2.1:9")
            .and_then(|()| probe.local_addr()),
    )
}

/// What the info server says about the interface, from its files: the MAC
/// must be read ([`HostError::Run`] when it cannot be, [`HostError::Setup`]
/// when the interface has none); the speed and the routes may be missing
/// (a link that is down, no route table): zeros.
#[cfg(target_os = "linux")]
pub fn interface_info(ifa: &IfAddr) -> Result<NetInfo, HostError> {
    let sys = format!("/sys/class/net/{}", ifa.name);
    let mac = std::fs::read_to_string(format!("{sys}/address"))
        .map_err(|e| run_failed(format!("the interface's MAC address: {e}")))?;
    let speed = std::fs::read_to_string(format!("{sys}/speed")).ok();
    let routes = std::fs::read_to_string("/proc/net/route").ok();
    daemon::net_info(ifa, &mac, speed.as_deref(), routes.as_deref()).map_err(setup)
}

#[cfg(windows)]
pub(crate) mod windows;
#[cfg(windows)]
pub use windows::interface_info;
#[cfg(all(windows, test))]
pub(crate) use windows::win_nic;

/// The TX flows' cookies: any start differing between runs.
fn seed() -> u64 {
    (realtime_ns() as u64) ^ ((monotonic_ns() as u64) << 21)
}

/// The four servers' sockets, each bound to `ip` (in `rt`).
fn bind_sockets(
    rt: &Runtime,
    ip: std::net::Ipv4Addr,
    cfg: &DeviceConfig,
) -> Result<Sockets, HostError> {
    let (arc_port, cmc_port, flows_port) = (cfg.arc_port, cfg.cmc_port, cfg.flows_port);
    rt.block_on(async {
        let bind = |port: u16, what: &'static str| async move {
            let sock = UdpSocket::bind((ip, port))
                .await
                .map_err(|e| run_failed(format!("{what} on UDP {port}: {e}")))?;
            crate::winsock::no_connreset(&sock)
                .map_err(|e| run_failed(format!("{what} on UDP {port}: {e}")))?;
            Ok::<_, HostError>(sock)
        };
        // The info port is one per address: held by another device of ours
        // on it, this device is a member of that one's (`info::share`).
        let info = match UdpSocket::bind((ip, info::REQUEST_PORT)).await {
            Ok(sock) => {
                crate::winsock::no_connreset(&sock)
                    .map_err(|e| run_failed(format!("the info server: {e}")))?;
                // Bound to the address, as inferno's; the multicasts'
                // interface is named too (Windows may not route them by the
                // bound address).
                socket2::SockRef::from(&sock)
                    .set_multicast_if_v4(&ip)
                    .map_err(|e| {
                        run_failed(format!("the info server's multicast interface: {e}"))
                    })?;
                Some(sock)
            }
            Err(e) => {
                member_or(e).map_err(|e| {
                    run_failed(format!(
                        "the info server on UDP {}: {e}",
                        info::REQUEST_PORT
                    ))
                })?;
                info!(
                    port = info::REQUEST_PORT,
                    "the info port is held on this address: a member of its manager"
                );
                None
            }
        };
        Ok(Sockets {
            arc: bind(arc_port, "ARC").await?,
            cmc: bind(cmc_port, "CMC").await?,
            info,
            flows: bind(flows_port, "flow control").await?,
        })
    })
}

/// Whether binding the info port failed because another socket holds it on
/// the address (another device of ours: this one joins it as a member).
pub fn info_port_held(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::AddrInUse
}

/// A failed bind of the info port: `Ok` when it is held on the address (the
/// device joins as a member), else the error.
pub fn member_or(e: std::io::Error) -> Result<(), std::io::Error> {
    if info_port_held(&e) { Ok(()) } else { Err(e) }
}

/// The device set up and bound, nothing served yet (see the module).
pub struct Prepared {
    device: Device,
    net: NetInfo,
    rt: Runtime,
    sockets: Sockets,
    /// The media clock the poller keeps (the media side reads it).
    mono: Arc<MonoClock>,
    /// The linked clock's selector side ([`ClockSource::Linked`]).
    linked: Option<Arc<Linked>>,
    source: ClockSource,
    media: Arc<Media>,
    #[cfg(unix)]
    listener: Option<UnixListener>,
}

/// [`prepare_with`] on the default clock ([`ClockSource::Linked`]): the
/// production device.
pub fn prepare(opts: &Options, engine_tx: &[usize]) -> Result<Prepared, HostError> {
    prepare_with(opts, engine_tx, ClockSource::default())
}

/// The device of `opts` set up, nothing served yet: the config, its state
/// file (writable), the interface and its address, its network info, the
/// runtime, the sockets of ARC, CMC, the info server and flow control on the
/// address, the media side on it with an engine ring on each TX channel
/// index (from 0) in `engine_tx` (the standalone device passes none; an
/// index the device lacks is [`HostError::Setup`]), and the control socket
/// at `opts.control`. Glue: every decision is [`read_config`]'s,
/// [`load_device`]'s, [`daemon::wait_for_interface`]'s, [`interface_info`]'s
/// and [`Media::new`]'s. The media clock follows `source`
/// ([`ClockSource::System`] is for tests only).
pub fn prepare_with(
    opts: &Options,
    engine_tx: &[usize],
    source: ClockSource,
) -> Result<Prepared, HostError> {
    prepare_config(
        read_config(&opts.config)?,
        &opts.place,
        Some(&opts.control),
        engine_tx,
        source,
    )
}

/// [`prepare_with`] for a config the caller built (an app hosting the
/// device: [`crate::embed`]): the control socket at `control`, none with
/// `None` (Windows has none either way).
///
/// The place is waited for while it is not there (an interface still
/// getting its address at boot or logon, dstack #225): the call returns
/// only when it is, or on another error ([`daemon::wait_for_interface`]).
pub fn prepare_config(
    cfg: DeviceConfig,
    place: &Place,
    control: Option<&Path>,
    engine_tx: &[usize],
    source: ClockSource,
) -> Result<Prepared, HostError> {
    prepare_config_waiting(cfg, place, PlaceWait::Forever, control, engine_tx, source)
}

/// [`prepare_config`] with a place not there yet met as `wait` says
/// ([`daemon::resolve_place`], dstack #230): [`PlaceWait::Now`] fails at
/// once (dlink's `Link::start`).
pub(crate) fn prepare_config_waiting(
    cfg: DeviceConfig,
    place: &Place,
    wait: PlaceWait<'_>,
    control: Option<&Path>,
    engine_tx: &[usize],
    source: ClockSource,
) -> Result<Prepared, HostError> {
    prepare_on(cfg, place, control, engine_tx, source, |place| {
        daemon::resolve_place(place, wait, interfaces, std::thread::sleep)
    })
}

/// Sets the device's flow port bases as it binds them (dstack #264): a
/// configured base wins, an unset one is its slot's fixed block in the
/// dstack range ([`crate::device::ports::flow_bases`]); never the OS's
/// ports. Logs the ports and each WARN; a device with no slot and a base
/// unset, or RX and TX blocks that overlap, is [`HostError::Setup`].
fn fix_flow_ports(cfg: &mut DeviceConfig) -> Result<(), HostError> {
    let bases = crate::device::ports::flow_bases(cfg.rx_port_base, cfg.tx_port_base, cfg.arc_port)
        .map_err(setup)?;
    for line in &bases.warnings {
        warn!("{line}");
    }
    // In range: flow_bases refuses blocks that overlap, and a configured
    // base past 65535 is refused by the config's own check before this.
    let last = |base: u16| crate::media::rx::last_port(base).unwrap_or(u16::MAX);
    info!(
        slot = ?bases.slot,
        rx_ports = format!("{}-{}", bases.rx, last(bases.rx)),
        tx_ports = format!("{}-{}", bases.tx, last(bases.tx)),
        "the device's flow ports: fixed, its slot's block in the dstack range unless configured"
    );
    cfg.rx_port_base = Some(bases.rx);
    cfg.tx_port_base = Some(bases.tx);
    Ok(())
}

/// [`prepare_config`] with the place resolved by `resolve` (the tests'
/// scripted address source).
pub(crate) fn prepare_on(
    cfg: DeviceConfig,
    place: &Place,
    control: Option<&Path>,
    engine_tx: &[usize],
    source: ClockSource,
    resolve: impl FnOnce(&Place) -> Result<IfAddr, String>,
) -> Result<Prepared, HostError> {
    let mut device = load_device(cfg)?;
    fix_flow_ports(&mut device.cfg)?;
    // One timing profile per process: the media threads' waits and
    // priorities read it.
    if !crate::clock::set_profile(device.cfg.timing) {
        return Err(setup(format!(
            "the timing profile {:?} differs from the {:?} another device of this process set",
            device.cfg.timing,
            crate::clock::profile()
        )));
    }
    crate::clock::set_spin_tail_ns(device.cfg.spin_tail_ns);
    crate::clock::set_spin_escape_ms(device.cfg.spin_escape_ms);
    if let Some(line) =
        crate::clock::spin_escape_line(device.cfg.spin_escape_ms, crate::clock::Os::current())
    {
        info!("{line}");
    }
    if let Some(&tx) = engine_tx.iter().find(|&&tx| tx >= device.cfg.tx.len()) {
        return Err(setup(format!(
            "the engine's TX channel {} is past the device's {} TX channels",
            tx + 1,
            device.cfg.tx.len()
        )));
    }
    // An address not there yet is waited for; a refused one is exit 1.
    let ifa = resolve(place).map_err(run_failed)?;
    let net = interface_info(&ifa)?;
    info!(
        version = VERSION,
        build = BUILD,
        rx = device.cfg.rx.len(),
        tx = device.cfg.tx.len(),
        engine_tx = engine_tx.len(),
        rate = device.cfg.sample_rate,
        encoding = device.cfg.encoding,
        latency_ns = device.cfg.latency_ns,
        link_mbit_s = net.link_speed,
        gateway = !net.gateway.is_unspecified(),
        clock = match source {
            ClockSource::Dsync => "dsync",
            ClockSource::Linked => "linked (dsync's follower, else the service)",
            ClockSource::System => "system (test only)",
        },
        timing = ?device.cfg.timing,
        windows_wait = ?crate::clock::wait_mode_for(device.cfg.timing),
        "Dante device starting (no PTP sent)"
    );
    if let Some(w) = daemon::gateway_warning(net.gateway) {
        warn!("{w}");
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(run_failed)?;
    let sockets = bind_sockets(&rt, net.ip, &device.cfg)?;
    info!(
        arc = device.cfg.arc_port,
        cmc = device.cfg.cmc_port,
        info = info::REQUEST_PORT,
        info_shared = sockets.info.is_none(),
        flows = device.cfg.flows_port,
        process_id = device.cfg.process_id,
        info_group_port = info::INFO_PORT,
        heartbeat_port = info::HEARTBEAT_PORT,
        "listening on the interface's address"
    );
    // The media clock: PTP time from dsync on the box, polled once a
    // second for the device's life (from the start).
    let mono = Arc::new(MonoClock::new());
    // dlink's: dsync's follower when locked, the service otherwise.
    let linked = (source == ClockSource::Linked).then(|| {
        // Seeded with the service's frequency word when it is locked and
        // settled (dstack #68): settled in 37 s instead of 70.
        let seed = rt.block_on(crate::clock::linked::service_seed(crate::clock::DSYNC));
        let (internal, keep) = crate::clock::linked::start_internal(&ifa.name, seed);
        Arc::new(Linked::new(internal, Arc::clone(&mono), keep))
    });
    let ptp: Arc<dyn crate::clock::PtpClock> = match &linked {
        Some(l) => l.clone(),
        None => mono.clone(),
    };
    let clock = Clock {
        ptp,
        freq: Arc::new(AtomicI32::new(0)),
        stats: Arc::new(ClockStats::default()),
    };
    if source == ClockSource::System {
        // Test only: PTP time is this machine's clock from now on.
        let k_ns = realtime_ns() - monotonic_ns();
        mono.set_k(k_ns);
        clock.stats.mark_system();
        warn!(
            "media clock: the TEST system clock; PTP time is this machine's clock, not the \
             network's: never for a real Dante network"
        );
    }
    let media = Media::new(&device, net.ip, clock, seed(), engine_tx)
        .map_err(|e| run_failed(format!("the flow sockets: {e}")))?;
    #[cfg(unix)]
    let listener = match control {
        Some(path) => {
            let _in_rt = rt.enter();
            Some(ctl::bind_control(path).map_err(|e| {
                run_failed(format!(
                    "the control socket {}: {e} (the unit's RuntimeDirectory makes its folder)",
                    path.display()
                ))
            })?)
        }
        None => None,
    };
    #[cfg(windows)]
    let _ = control;
    Ok(Prepared {
        device,
        net,
        rt,
        sockets,
        mono,
        linked,
        source,
        media: Arc::new(media),
        #[cfg(unix)]
        listener,
    })
}

/// What the device's thread and its host share.
#[derive(Default)]
struct Control {
    /// Asks serving to end (a permit, so a request before serving waits
    /// for it is kept).
    stop: Notify,
    /// Why serving ended by itself; set once serving returned (its RX
    /// flows stopped, its media threads ended: within 2 s of the end),
    /// before the goodbyes.
    ended: OnceLock<String>,
}

/// Serving's end as the thread reports it: an error is kept as the reason
/// serving ended by itself ([`Host::ended`]) and is the thread's
/// [`HostError::Run`]; an end that was asked for (a stop, a signal) is
/// none.
fn settle(control: &Control, served: Result<(), ServeError>) -> Result<(), HostError> {
    served.map_err(|e| {
        let why = e.to_string();
        let _first = control.ended.set(why.clone());
        HostError::Run(why)
    })
}

impl Prepared {
    /// The media side: the engine's pump reads its RX rings and writes its
    /// TX side's engine rings; [`Media::attach_engine`] shows the pump on
    /// the control socket.
    pub fn media(&self) -> Arc<Media> {
        Arc::clone(&self.media)
    }

    /// The device's config as it runs (the engine maps its channels
    /// against it): the state file's values that hold over the config's
    /// applied, such as a latency Dante Controller set.
    pub fn config(&self) -> &DeviceConfig {
        &self.device.cfg
    }

    /// Starts serving: the signals as `signals` says, the mDNS daemon on
    /// the address's interface only, and the `dstack-device` thread (see
    /// the module). Glue: the mDNS daemon, the signals and the thread.
    pub fn start(self, signals: Signals) -> Result<Host, HostError> {
        let Prepared {
            device,
            net,
            rt,
            sockets,
            mono,
            linked,
            source,
            media,
            #[cfg(unix)]
            listener,
        } = self;
        let mut device = device;
        let (watch, local) = {
            let _in_rt = rt.enter();
            (Watch::new(signals)?, open_local(&mut device))
        };
        let mdns = ServiceDaemon::new().map_err(|e| run_failed(format!("the mDNS daemon: {e}")))?;
        // Only the served address's interface, IPv4: no other network sees
        // the records (mdns-sd applies the selections in order).
        let selected = mdns
            .disable_interface(IfKind::All)
            .and_then(|()| mdns.enable_interface(IfKind::Addr(IpAddr::V4(net.ip))));
        if let Err(e) = selected {
            let _withdrawn = goodbye(&mdns);
            return Err(run_failed(format!("the mDNS interface: {e}")));
        }
        // dstack #289: who sends how much mDNS on the leg, one meter per
        // process and address; the device runs on without it (logged).
        match crate::discover::senders::Meter::shared(net.ip) {
            Ok(meter) => media.set_mdns_meter(meter),
            Err(e) => warn!(%e, "the mDNS meter is not started: no mDNS senders in the status"),
        }
        let control = Arc::new(Control::default());
        #[cfg(windows)]
        let net_ip = net.ip;
        let served = Served {
            device,
            net,
            sockets,
            mono,
            linked,
            source,
            media,
            local,
            #[cfg(unix)]
            listener,
        };
        let kept = mdns.clone();
        let theirs = Arc::clone(&control);
        let announcer = Announcer::new(mdns);
        // Windows: our records answered from the device's own address too
        // (a SkipAsSource address's answers leave from the primary one;
        // dstack #1 problem E); it ends with the device's thread.
        #[cfg(windows)]
        let own = crate::announce::responder::Responder::start(net_ip, announcer.published())
            .map_err(|e| warn!(%e, "mDNS answers from the device's address not started"))
            .ok();
        let thread = std::thread::Builder::new()
            .name("dstack-device".to_string())
            .spawn(move || {
                #[cfg(windows)]
                let _own = own;
                device_thread(rt, served, announcer, &theirs, watch)
            });
        match thread {
            Ok(thread) => Ok(Host { control, thread }),
            Err(e) => {
                let _withdrawn = goodbye(&kept);
                Err(run_failed(format!("the device thread: {e}")))
            }
        }
    }
}

/// What serving takes from [`Prepared`].
struct Served {
    device: Device,
    net: NetInfo,
    sockets: Sockets,
    mono: Arc<MonoClock>,
    /// The linked clock's selector side ([`ClockSource::Linked`]).
    linked: Option<Arc<Linked>>,
    source: ClockSource,
    media: Arc<Media>,
    /// The local status endpoint ([`crate::ctl::local`]), when it listens.
    local: Option<tokio::net::TcpListener>,
    #[cfg(unix)]
    listener: Option<UnixListener>,
}

/// The local status endpoint for dtray (#104, [`crate::ctl::local`]): a listener
/// on 127.0.0.1, its port kept in `device` for the ARC record. Without it
/// the device runs on, unseen by dtray (logged). Call it inside the
/// device's runtime.
fn open_local(device: &mut Device) -> Option<tokio::net::TcpListener> {
    match crate::ctl::local::bind().and_then(|l| Ok((l.local_addr()?.port(), l))) {
        Ok((port, listener)) => {
            info!(port, "local status endpoint on 127.0.0.1 (dtray reads it)");
            device.status_port = Some(port);
            Some(listener)
        }
        Err(e) => {
            warn!(error = %e, "no local status endpoint: dtray will not list this device");
            None
        }
    }
}

/// The signals as registered (in the device's runtime).
#[cfg(unix)]
enum Watch {
    Stop {
        term: Signal,
        int: Signal,
    },
    Report {
        term: Signal,
        int: Signal,
        flag: Arc<AtomicBool>,
    },
    /// No handler: only the host's stop.
    Host,
}

#[cfg(unix)]
impl Watch {
    /// Registers SIGTERM and SIGINT (inside the runtime), except for
    /// [`Signals::Host`], which registers nothing.
    fn new(signals: Signals) -> Result<Watch, HostError> {
        let both = || -> Result<(Signal, Signal), HostError> {
            let term = signal(SignalKind::terminate()).map_err(run_failed)?;
            let int = signal(SignalKind::interrupt()).map_err(run_failed)?;
            Ok((term, int))
        };
        Ok(match signals {
            Signals::Stop => {
                let (term, int) = both()?;
                Watch::Stop { term, int }
            }
            Signals::Report(flag) => {
                let (term, int) = both()?;
                Watch::Report { term, int, flag }
            }
            Signals::Host => Watch::Host,
        })
    }

    /// The future that ends serving: a signal (for [`Signals::Stop`]) or
    /// the host's stop. For [`Signals::Report`] a task sets the flag at
    /// each signal while the device serves. Glue: tokio's signals.
    async fn stop(self, control: &Control) {
        match self {
            Watch::Stop { mut term, mut int } => {
                tokio::select! {
                    _ = term.recv() => info!("SIGTERM: stopping"),
                    _ = int.recv() => info!("SIGINT: stopping"),
                    () = control.stop.notified() => info!("the device's stop was asked"),
                }
            }
            Watch::Report { term, int, flag } => {
                tokio::spawn(report(term, int, flag));
                control.stop.notified().await;
                info!("the device's stop was asked");
            }
            Watch::Host => {
                control.stop.notified().await;
                info!("the device's stop was asked");
            }
        }
    }
}

/// Sets `flag` at every SIGTERM or SIGINT (the host stops the device).
#[cfg(unix)]
async fn report(mut term: Signal, mut int: Signal, flag: Arc<AtomicBool>) {
    loop {
        let which = tokio::select! {
            Some(()) = term.recv() => "SIGTERM",
            Some(()) = int.recv() => "SIGINT",
            else => return,
        };
        info!(
            signal = which,
            "a stop was asked: the host stops the device"
        );
        flag.store(true, Ordering::Release);
    }
}

/// On Windows: a console Ctrl+C (or the console's close) in place of the
/// signals.
#[cfg(windows)]
enum Watch {
    Stop,
    Report(Arc<AtomicBool>),
    Host,
}

#[cfg(windows)]
impl Watch {
    fn new(signals: Signals) -> Result<Watch, HostError> {
        Ok(match signals {
            Signals::Stop => Watch::Stop,
            Signals::Report(flag) => Watch::Report(flag),
            Signals::Host => Watch::Host,
        })
    }

    /// The future that ends serving: Ctrl+C (for [`Signals::Stop`]) or the
    /// host's stop; for [`Signals::Report`] Ctrl+C only sets the flag.
    async fn stop(self, control: &Control) {
        match self {
            Watch::Stop => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => info!("Ctrl+C: stopping"),
                    () = control.stop.notified() => info!("the device's stop was asked"),
                }
            }
            Watch::Report(flag) => {
                tokio::spawn(async move {
                    while tokio::signal::ctrl_c().await.is_ok() {
                        info!("Ctrl+C: the host stops the device");
                        flag.store(true, Ordering::Release);
                    }
                });
                control.stop.notified().await;
                info!("the device's stop was asked");
            }
            Watch::Host => {
                control.stop.notified().await;
                info!("the device's stop was asked");
            }
        }
    }
}

/// The `dstack-device` thread: serving in the runtime until stopped, then
/// the goodbyes whatever ended serving (no record outlives the device).
/// Glue: the runtime and the mDNS daemon.
fn device_thread(
    rt: Runtime,
    served: Served,
    mut announcer: Announcer<ServiceDaemon>,
    control: &Control,
    watch: Watch,
) -> Result<(), HostError> {
    let served = rt.block_on(serve_device(served, &mut announcer, control, watch));
    let served = settle(control, served);
    let goodbye = goodbye(announcer.registry());
    served?;
    goodbye
}

/// The linked clock's selector: once a second, for the device's life.
async fn select_source(linked: Arc<Linked>, stats: Arc<crate::clock::ClockStats>) {
    let mut every = tokio::time::interval(Duration::from_secs(1));
    loop {
        every.tick().await;
        linked.tick();
        stats.set_linked(linked.source());
        stats.set_follower(linked.follower_counts());
    }
}

/// The media clock's poller, then [`daemon::serve`] until `watch`'s stop.
async fn serve_device(
    s: Served,
    announcer: &mut Announcer<ServiceDaemon>,
    control: &Control,
    watch: Watch,
) -> Result<(), ServeError> {
    if matches!(s.source, ClockSource::Dsync | ClockSource::Linked) {
        tokio::spawn(poll_dsync(
            s.mono,
            Arc::clone(&s.media.freq),
            Arc::clone(&s.media.clock_stats),
        ));
    }
    if let Some(linked) = s.linked {
        tokio::spawn(select_source(linked, Arc::clone(&s.media.clock_stats)));
    }
    if let Some(listener) = s.local {
        let media = Arc::clone(&s.media);
        tokio::spawn(async move {
            if let Err(e) = crate::ctl::local::serve(listener, media).await {
                warn!(error = %e, "the local status endpoint stopped");
            }
        });
    }
    #[cfg(unix)]
    let ctl_server = {
        let media = Arc::clone(&s.media);
        async move {
            match s.listener {
                Some(listener) => ctl::serve_control(listener, media).await,
                None => std::future::pending().await,
            }
        }
    };
    // No control socket on Windows (dasio's tray reads the shared memory).
    #[cfg(windows)]
    let ctl_server = std::future::pending::<std::io::Result<()>>();
    daemon::serve(
        s.sockets,
        Arc::new(Mutex::new(s.device)),
        announcer,
        s.net,
        Multicast::DANTE,
        s.media,
        ctl_server,
        watch.stop(control),
    )
    .await
}

/// Shuts the mDNS daemon down, which withdraws every record (goodbyes)
/// before it says so; waits at most [`GOODBYE_WAIT`] for that.
fn goodbye(mdns: &ServiceDaemon) -> Result<(), HostError> {
    let status = mdns
        .shutdown()
        .map_err(|e| run_failed(format!("the mDNS shutdown: {e}")))?;
    match status.recv_timeout(GOODBYE_WAIT) {
        Ok(DaemonStatus::Shutdown) => {
            info!("mDNS records withdrawn (goodbyes sent); stopped");
            Ok(())
        }
        Ok(other) => Err(run_failed(format!("the mDNS shutdown answered {other:?}"))),
        Err(e) => Err(run_failed(format!(
            "the mDNS shutdown was not confirmed within {} s: {e}",
            GOODBYE_WAIT.as_secs()
        ))),
    }
}

/// How [`Host::stop`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// Served, stopped, goodbyes sent.
    Done,
    /// The device's thread ended with this error (serving failed before, or
    /// the goodbyes were not confirmed).
    Failed(String),
    /// Still running at the end of the wait: left to end, never killed.
    StillRunning,
}

/// The running device (see the module).
pub struct Host {
    control: Arc<Control>,
    thread: JoinHandle<Result<(), HostError>>,
}

/// The device thread's end when it panicked.
const PANICKED: &str = "the device thread panicked";

impl Host {
    /// Why serving ended by itself (a server that stopped, threads that did
    /// not start, …), once serving returned and before the goodbyes; `None`
    /// while it serves or when it was asked to end.
    pub fn ended(&self) -> Option<String> {
        self.control.ended.get().cloned()
    }

    /// Asks serving to end (every RX flow stopped at its transmitter, the
    /// media threads ended, the goodbyes) and waits up to `within` for the
    /// device's thread: [`Stopped::Done`], [`Stopped::Failed`] with its
    /// error, or [`Stopped::StillRunning`] (a warning; the thread is left to
    /// end, never killed).
    pub fn stop(self, within: Duration) -> Stopped {
        self.control.stop.notify_one();
        let deadline = Instant::now() + within;
        while !self.thread.is_finished() {
            if Instant::now() >= deadline {
                warn!(
                    wait_ms = within.as_millis(),
                    "the device did not end within the wait: left to end"
                );
                return Stopped::StillRunning;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        match self.thread.join() {
            Ok(Ok(())) => Stopped::Done,
            Ok(Err(e)) => Stopped::Failed(e.to_string()),
            Err(_) => Stopped::Failed(PANICKED.to_string()),
        }
    }

    /// Waits for the device's thread to end by itself (a signal under
    /// [`Signals::Stop`], or serving that failed): its result.
    pub fn wait(self) -> Result<(), HostError> {
        self.thread
            .join()
            .unwrap_or_else(|_| Err(HostError::Run(PANICKED.to_string())))
    }
}

#[cfg(test)]
mod tests;
