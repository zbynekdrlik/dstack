//! The QoS2 calls (qwave.dll) that mark a TX flow socket EF on Windows.
//! Windows-only, so outside the mutation gate (`.cargo/mutants.toml`);
//! the decisions are [`super::changes`] and [`super::verdict`], tested on
//! every platform, and CI runs this glue on Windows.

use std::net::{SocketAddrV4, UdpSocket};
use std::os::windows::io::AsRawSocket;

use tracing::{debug, info, warn};
use windows_sys::Win32::Foundation::{GetLastError, HANDLE};
use windows_sys::Win32::NetworkManagement::QoS::{
    QOS_NON_ADAPTIVE_FLOW, QOS_VERSION, QOSAddSocketToFlow, QOSCloseHandle, QOSCreateHandle,
    QOSRemoveSocketFromFlow, QOSSetFlow, QOSSetOutgoingDSCPValue, QOSTrafficTypeAudioVideo,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR, SOCKADDR_IN};

use super::{
    POLICY_KEY, POLICY_VALUES, Policy, PolicyValue, Verdict, marking_policy, unmarked_note, verdict,
};
use crate::media::tx::DSCP_EF;
use crate::media::tx::MAX_FLOWS;

/// The QoS2 handle and each slot's flow.
#[derive(Debug)]
pub struct Qos {
    /// The handle as an address (a raw `HANDLE` is not `Send`); 0: none.
    handle: usize,
    /// Each slot's QoS2 flow id; 0: none.
    flows: [u32; MAX_FLOWS],
    /// The last DSCP verdict, any slot.
    pub last: Option<Verdict>,
    /// Whether the outcome was logged (once each).
    told_set: bool,
    told_refused: bool,
}

impl Qos {
    /// Opens the QoS2 handle; without one (the qWave service missing) the
    /// marks are skipped, with a warning.
    pub fn new() -> Self {
        let version = QOS_VERSION {
            MajorVersion: 1,
            MinorVersion: 0,
        };
        let mut handle: HANDLE = std::ptr::null_mut();
        // SAFETY: both pointers are valid for the call.
        let ok = unsafe { QOSCreateHandle(&version, &mut handle) } != 0;
        if !ok {
            // SAFETY: no precondition.
            let code = unsafe { GetLastError() };
            if let Some(note) = policy_note() {
                info!(code, "{note}");
            } else {
                warn!(
                    code,
                    "TX audio leaves unmarked (DSCP 0): no QoS2 handle (is the qWave service there?); a QoS policy for this program with DSCP 46 marks it instead (docs/windows-box.md)"
                );
            }
        }
        Qos {
            handle: if ok { handle as usize } else { 0 },
            flows: [0; MAX_FLOWS],
            last: None,
            told_set: false,
            told_refused: false,
        }
    }

    /// Marks slot `slot`'s `socket` EF for `dest`, leaving the flow of its
    /// previous destination; `None` only leaves it.
    pub fn mark(&mut self, slot: usize, socket: &UdpSocket, dest: Option<SocketAddrV4>) {
        if self.handle == 0 || slot >= MAX_FLOWS {
            return;
        }
        let handle = self.handle as HANDLE;
        let raw = socket.as_raw_socket() as usize;
        if self.flows[slot] != 0 {
            // SAFETY: a handle and flow this state opened, a live socket.
            unsafe { QOSRemoveSocketFromFlow(handle, raw, self.flows[slot], 0) };
            self.flows[slot] = 0;
        }
        let Some(dest) = dest else {
            return;
        };
        // SAFETY: a zeroed SOCKADDR_IN is valid; the fields are set below.
        let mut sa: SOCKADDR_IN = unsafe { std::mem::zeroed() };
        sa.sin_family = AF_INET;
        sa.sin_port = dest.port().to_be();
        sa.sin_addr.S_un.S_addr = u32::from(*dest.ip()).to_be();
        let mut id = 0u32;
        // SAFETY: the handle is open, the socket live, `sa` an AF_INET
        // address that outlives the call, `id` writable.
        let added = unsafe {
            QOSAddSocketToFlow(
                handle,
                raw,
                (&raw const sa).cast::<SOCKADDR>(),
                QOSTrafficTypeAudioVideo,
                QOS_NON_ADAPTIVE_FLOW,
                &mut id,
            )
        } != 0;
        if !added {
            // SAFETY: no precondition.
            let code = unsafe { GetLastError() };
            if super::local_destination(code) {
                info!(slot, %dest, "TX flow to a destination on this PC: not marked (it never leaves the NIC)");
            } else {
                warn!(slot, %dest, code, "TX flow left unmarked: QOSAddSocketToFlow failed");
            }
            self.last = Some(Verdict::Failed(code));
            return;
        }
        self.flows[slot] = id;
        let dscp = u32::from(DSCP_EF);
        // SAFETY: an open handle and flow; the value outlives the call.
        let set = unsafe {
            QOSSetFlow(
                handle,
                id,
                QOSSetOutgoingDSCPValue,
                4,
                (&raw const dscp).cast(),
                0,
                std::ptr::null_mut(),
            )
        } != 0;
        // SAFETY: no precondition.
        let v = verdict((!set).then(|| unsafe { GetLastError() }));
        self.last = Some(v);
        match v {
            Verdict::Set if !self.told_set => {
                self.told_set = true;
                info!(slot, %dest, dscp, "TX audio marked DSCP 46 (EF) through QoS2");
            }
            Verdict::NotAdmin if !self.told_refused => {
                self.told_refused = true;
                if let Some(note) = policy_note() {
                    info!("{note}");
                } else {
                    warn!(
                        "TX audio leaves unmarked (DSCP 0): Windows sets a DSCP only for an administrator process; run as a service or admin, or add a QoS policy for this program with DSCP 46 (docs/windows-box.md)"
                    );
                }
            }
            Verdict::Failed(code) => {
                warn!(slot, %dest, code, "TX flow left unmarked: QOSSetFlow failed");
            }
            _ => {}
        }
    }
}

impl Drop for Qos {
    fn drop(&mut self) {
        if self.handle != 0 {
            // SAFETY: the handle this state opened; closing it ends its flows.
            unsafe { QOSCloseHandle(self.handle as HANDLE) };
        }
    }
}

/// The info line when a QoS policy marks this program's audio DSCP 46
/// (dstack #256); none: the caller warns.
fn policy_note() -> Option<String> {
    let exe = match std::env::current_exe() {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(e) => {
            debug!(error = %e, "this program's path unread: QoS policies not checked");
            return None;
        }
    };
    let policies = read_policies();
    debug!(
        count = policies.len(),
        exe, "QoS policies read from HKLM\\{POLICY_KEY}"
    );
    unmarked_note(marking_policy(&policies, &exe))
}

/// The QoS policies under HKLM\[`POLICY_KEY`], with their
/// [`POLICY_VALUES`]; none when the key is absent (no policy).
pub(super) fn read_policies() -> Vec<Policy> {
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW,
    };
    let key = wide(POLICY_KEY);
    let mut root: HKEY = std::ptr::null_mut();
    // SAFETY: a NUL-terminated key name and a writable handle.
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, key.as_ptr(), 0, KEY_READ, &mut root) } != 0 {
        return Vec::new();
    }
    let mut policies = Vec::new();
    for index in 0u32.. {
        // A key name holds 255 characters at most.
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        // SAFETY: an open key; `name` holds `len` characters; the rest out
        // pointers are null (not wanted).
        let r = unsafe {
            RegEnumKeyExW(
                root,
                index,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if r != 0 {
            break;
        }
        let sub = &name[..(len as usize).min(name.len())];
        let values = POLICY_VALUES
            .iter()
            .filter_map(|v| read_value(root, sub, v).map(|x| ((*v).to_string(), x)))
            .collect();
        policies.push(Policy {
            name: String::from_utf16_lossy(sub),
            values,
        });
    }
    // SAFETY: the key this function opened.
    unsafe { RegCloseKey(root) };
    policies
}

/// The string or DWORD value `value` of `root`'s subkey `sub`.
fn read_value(
    root: windows_sys::Win32::System::Registry::HKEY,
    sub: &[u16],
    value: &str,
) -> Option<PolicyValue> {
    use windows_sys::Win32::System::Registry::{
        REG_DWORD, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
    };
    let sub: Vec<u16> = sub.iter().copied().chain(std::iter::once(0)).collect();
    let v = wide(value);
    let mut buf = [0u16; 512];
    let mut len = (buf.len() * 2) as u32;
    let mut kind = 0u32;
    // SAFETY: an open key, NUL-terminated names, `buf` is `len` writable
    // bytes, `kind` writable.
    let r = unsafe {
        RegGetValueW(
            root,
            sub.as_ptr(),
            v.as_ptr(),
            RRF_RT_REG_SZ | RRF_RT_REG_DWORD,
            &mut kind,
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if r != 0 {
        return None;
    }
    match kind {
        REG_DWORD => Some(PolicyValue::Number(
            u32::from(buf[0]) | (u32::from(buf[1]) << 16),
        )),
        REG_SZ => {
            let n = (len as usize / 2).min(buf.len());
            Some(PolicyValue::Text(
                String::from_utf16_lossy(&buf[..n])
                    .trim_end_matches('\0')
                    .to_string(),
            ))
        }
        _ => None,
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
