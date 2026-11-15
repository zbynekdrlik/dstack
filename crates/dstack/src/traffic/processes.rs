// SPDX-License-Identifier: MIT OR Apache-2.0

//! The processes on the Dante NIC's address, for the traffic recorder's
//! dump (dstack #195): which programs hold TCP or UDP endpoints there, the
//! most first, with their CPU. Windows reads the endpoint tables through
//! IP Helper (`traffic/windows.rs`, off the media path, once a burst); the
//! ranking and the text are pure, here.

/// How many processes a dump names.
pub const TOP: usize = 5;

/// A process on the NIC's address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    /// Its executable's file name; empty when unreadable.
    pub name: String,
    /// Its TCP and UDP endpoints on the address.
    pub endpoints: usize,
    /// Its CPU time since it started, ms; `None` when unreadable.
    pub cpu_ms: Option<u64>,
}

/// The `TOP` processes holding the most of `owners` (an endpoint's owner
/// pid each), most first, ties by pid; with their endpoint counts.
pub fn rank(owners: impl IntoIterator<Item = u32>) -> Vec<(u32, usize)> {
    let mut counts = std::collections::BTreeMap::<u32, usize>::new();
    for pid in owners {
        *counts.entry(pid).or_default() += 1;
    }
    let mut out: Vec<(u32, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out.truncate(TOP);
    out
}

/// The processes as the dump shows them, `, ` between:
/// `<name>(<pid>) endpoints=<n> cpu_ms=<ms>` (`?` for an unread name or
/// time); `none` for no process.
pub fn text(processes: &[Process]) -> String {
    if processes.is_empty() {
        return String::from("none");
    }
    processes
        .iter()
        .map(|p| {
            format!(
                "{}({}) endpoints={} cpu_ms={}",
                if p.name.is_empty() { "?" } else { &p.name },
                p.pid,
                p.endpoints,
                p.cpu_ms
                    .map_or_else(|| String::from("?"), |v| v.to_string()),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The file name of the executable path `path` (the last `\` or `/`
/// part).
pub fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// The processes with endpoints on the address `ip`, as [`text`] shows
/// them: read on Windows; elsewhere not read.
#[cfg(windows)]
pub fn on_address(ip: std::net::Ipv4Addr) -> String {
    text(&super::windows::processes_on(ip))
}

/// The processes with endpoints on the address `_ip`: not read on this OS.
#[cfg(not(windows))]
pub fn on_address(_ip: std::net::Ipv4Addr) -> String {
    String::from(NOT_READ)
}

/// What a dump names for the processes off Windows.
pub const NOT_READ: &str = "not read on this OS";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rank_counts_owners_and_keeps_the_top_five() {
        let owners = [7, 3, 7, 9, 3, 7, 1, 2, 4, 9, 5, 5];
        assert_eq!(rank(owners), vec![(7, 3), (3, 2), (5, 2), (9, 2), (1, 1)]);
        assert_eq!(rank([]), vec![]);
        assert_eq!(rank([4, 4]), vec![(4, 2)]);
    }

    #[test]
    fn the_text_names_each_process_or_none() {
        let p = |pid, name: &str, endpoints, cpu_ms| Process {
            pid,
            name: name.to_string(),
            endpoints,
            cpu_ms,
        };
        assert_eq!(text(&[]), "none");
        assert_eq!(
            text(&[p(12, "web.exe", 40, Some(3500)), p(4, "", 2, None)]),
            "web.exe(12) endpoints=40 cpu_ms=3500, ?(4) endpoints=2 cpu_ms=?"
        );
    }

    #[test]
    fn the_file_name_is_the_last_part() {
        assert_eq!(file_name(r"C:\Program Files\A\a b.exe"), "a b.exe");
        assert_eq!(file_name("/usr/bin/x"), "x");
        assert_eq!(file_name("plain"), "plain");
    }

    #[cfg(not(windows))]
    #[test]
    fn off_windows_the_processes_are_not_read() {
        assert_eq!(on_address(std::net::Ipv4Addr::LOCALHOST), NOT_READ);
    }

    /// The real read: this test's own UDP socket on 127.0.0.1 makes this
    /// process one of the owners (Windows CI).
    #[cfg(windows)]
    #[test]
    fn on_windows_our_own_socket_names_this_process() {
        let _sockets: Vec<_> = (0..40)
            .map(|_| std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
            .collect();
        let found = super::super::windows::processes_on(std::net::Ipv4Addr::LOCALHOST);
        let me = std::process::id();
        let ours = found
            .iter()
            .find(|p| p.pid == me)
            .expect("this process listed");
        assert!(ours.endpoints >= 40, "{ours:?}");
        assert!(!ours.name.is_empty() && ours.cpu_ms.is_some(), "{ours:?}");
    }
}
