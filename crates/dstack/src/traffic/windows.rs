//! The traffic watch's Windows glue: the adapter's sent-bytes counter
//! (`GetIfEntry2`'s `OutOctets`); for the recorder (dstack #195) the CPU
//! times and the processes with endpoints on the Dante NIC's address.
//! Windows-only, so outside the mutation gate (`.cargo/mutants.toml`); CI
//! tests it on Windows.

use std::net::Ipv4Addr;

use windows_sys::Win32::NetworkManagement::IpHelper::{GetIfEntry2, MIB_IF_ROW2};

/// The bytes the adapter holding `ip` has sent; `None` when none holds it
/// or Windows refuses the read.
pub fn nic_sent_bytes(ip: Ipv4Addr) -> Option<u64> {
    let (_, _, _, index, _) = crate::host::windows::win_nic::of_with_index(ip).ok()?;
    // SAFETY: a zeroed MIB_IF_ROW2 is a valid input; only InterfaceIndex
    // is read by the call, which fills the rest.
    let mut row: MIB_IF_ROW2 = unsafe { std::mem::zeroed() };
    row.InterfaceIndex = index;
    // SAFETY: `row` is a valid, writable MIB_IF_ROW2.
    (unsafe { GetIfEntry2(&mut row) } == 0).then_some(row.OutOctets)
}

/// A FILETIME as one 100 ns count.
fn ticks(t: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) + u64::from(t.dwLowDateTime)
}

/// This process's and the machine's CPU times now, in 100 ns
/// (`GetProcessTimes`, `GetSystemTimes`: the kernel time includes the
/// idle time); `None` when Windows refuses a read (dstack #195).
pub fn cpu_times() -> Option<super::recorder::CpuTimes> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetSystemTimes};
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut idle, mut kernel, mut user) = (zero, zero, zero);
    // SAFETY: three valid, writable FILETIMEs.
    if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
        return None;
    }
    let total = ticks(kernel) + ticks(user);
    let busy = total.saturating_sub(ticks(idle));
    // SAFETY: the current process's pseudo handle, never closed.
    let process = process_ticks(unsafe { GetCurrentProcess() })?;
    Some(super::recorder::CpuTimes {
        process,
        busy,
        total,
    })
}

/// A process's user + kernel time in 100 ns; `None` when refused.
fn process_ticks(handle: windows_sys::Win32::Foundation::HANDLE) -> Option<u64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetProcessTimes;
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: `handle` is a process handle the caller holds, and four
    // valid, writable FILETIMEs.
    let ok = unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
    (ok != 0).then(|| ticks(kernel) + ticks(user))
}

/// A table from `GetExtendedTcpTable` / `GetExtendedUdpTable` (`call`
/// with the buffer and its size): asked once for the size, then filled;
/// tried again while it grows meanwhile (a few times at most). `None` when
/// refused.
fn table(mut call: impl FnMut(*mut core::ffi::c_void, &mut u32) -> u32) -> Option<Vec<u64>> {
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    let mut size = 0u32;
    for _ in 0..4 {
        // u64 words keep the rows (u32 fields) aligned.
        let mut buf = vec![0u64; (size as usize).div_ceil(8).max(1)];
        let mut have = u32::try_from(buf.len() * 8).ok()?;
        match call(buf.as_mut_ptr().cast(), &mut have) {
            NO_ERROR => return Some(buf),
            ERROR_INSUFFICIENT_BUFFER => size = have.saturating_add(4096),
            _ => return None,
        }
    }
    None
}

/// The owner pids of the IPv4 TCP and UDP endpoints whose local address
/// is `ip` (IP Helper's owner-pid tables; wildcard listeners are not on
/// the NIC's address and do not count).
fn owners_on(ip: Ipv4Addr) -> Vec<u32> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCPROW_OWNER_PID, MIB_UDPROW_OWNER_PID,
        TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET;
    // dwLocalAddr holds the address's bytes in network order.
    let on = |addr: u32| Ipv4Addr::from(addr.to_ne_bytes()) == ip;
    let mut out = Vec::new();
    let tcp = table(|buf, size| {
        // SAFETY: `buf` holds `*size` writable bytes.
        unsafe { GetExtendedTcpTable(buf, size, 0, u32::from(AF_INET), TCP_TABLE_OWNER_PID_ALL, 0) }
    });
    if let Some(buf) = tcp {
        // SAFETY: a filled MIB_TCPTABLE_OWNER_PID: its count, then that
        // many rows, inside the buffer Windows filled.
        let rows = unsafe { rows::<MIB_TCPROW_OWNER_PID>(&buf) };
        out.extend(
            rows.iter()
                .filter(|r| on(r.dwLocalAddr))
                .map(|r| r.dwOwningPid),
        );
    }
    let udp = table(|buf, size| {
        // SAFETY: as above.
        unsafe { GetExtendedUdpTable(buf, size, 0, u32::from(AF_INET), UDP_TABLE_OWNER_PID, 0) }
    });
    if let Some(buf) = udp {
        // SAFETY: a filled MIB_UDPTABLE_OWNER_PID, as above.
        let rows = unsafe { rows::<MIB_UDPROW_OWNER_PID>(&buf) };
        out.extend(
            rows.iter()
                .filter(|r| on(r.dwLocalAddr))
                .map(|r| r.dwOwningPid),
        );
    }
    out
}

/// The rows of an owner-pid table in `buf`: its u32 count, then the rows
/// (each 4-byte aligned, starting at byte 4).
///
/// # Safety
/// `buf` holds a table Windows filled: a count and that many `T` rows.
unsafe fn rows<T>(buf: &[u64]) -> &[T] {
    let base = buf.as_ptr().cast::<u8>();
    // SAFETY: the count is the table's first u32.
    let count = unsafe { base.cast::<u32>().read() } as usize;
    let max = (buf.len() * 8).saturating_sub(4) / std::mem::size_of::<T>();
    // SAFETY: the rows follow the count, inside the buffer (bounded by
    // `max` against a count past it).
    unsafe { std::slice::from_raw_parts(base.add(4).cast::<T>(), count.min(max)) }
}

/// A process's executable file name and CPU time (ms); empty and `None`
/// when it cannot be opened (another user's protected process).
fn name_and_cpu(pid: u32) -> (String, Option<u64>) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    // SAFETY: a plain open by pid; a null handle is a refusal.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return (String::new(), None);
    }
    let mut path = [0u16; 520];
    let mut len = path.len() as u32;
    // SAFETY: `path` holds `len` writable u16s.
    let named = unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, path.as_mut_ptr(), &mut len)
    };
    let name = if named != 0 {
        let full = String::from_utf16_lossy(&path[..len as usize]);
        super::processes::file_name(&full).to_string()
    } else {
        String::new()
    };
    let cpu_ms = process_ticks(handle).map(|t| t / 10_000);
    // SAFETY: the handle opened above, closed once.
    unsafe { CloseHandle(handle) };
    (name, cpu_ms)
}

/// The processes holding the most TCP and UDP endpoints on `ip`
/// ([`super::processes::rank`]), with their names and CPU (dstack #195).
pub fn processes_on(ip: Ipv4Addr) -> Vec<super::processes::Process> {
    super::processes::rank(owners_on(ip))
        .into_iter()
        .map(|(pid, endpoints)| {
            let (name, cpu_ms) = name_and_cpu(pid);
            super::processes::Process {
                pid,
                name,
                endpoints,
                cpu_ms,
            }
        })
        .collect()
}
