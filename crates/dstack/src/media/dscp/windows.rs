//! The QoS2 calls (qwave.dll) that mark a TX flow socket EF on Windows.
//! Windows-only, so outside the mutation gate (`.cargo/mutants.toml`);
//! the decisions are [`super::changes`] and [`super::verdict`], tested on
//! every platform, and CI runs this glue on Windows.

use std::net::{SocketAddrV4, UdpSocket};
use std::os::windows::io::AsRawSocket;

use tracing::{debug, info, warn};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, GetLastError, HANDLE};
use windows_sys::Win32::NetworkManagement::QoS::{
    QOS_NON_ADAPTIVE_FLOW, QOS_VERSION, QOSAddSocketToFlow, QOSCloseHandle, QOSCreateHandle,
    QOSRemoveSocketFromFlow, QOSSetFlow, QOSSetOutgoingDSCPValue, QOSTrafficTypeAudioVideo,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR, SOCKADDR_IN};

use super::{POLICY_KEY, Policy, PolicyValue, Verdict, marking_policy, unmarked_note, verdict};
use crate::media::tx::DSCP_EF;
use crate::media::tx::MAX_FLOWS;
use crate::winreg;

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
    let policies = read_policies(winreg::HKEY_LOCAL_MACHINE, POLICY_KEY);
    debug!(
        count = policies.len(),
        exe, "QoS policies read from HKLM\\{POLICY_KEY}"
    );
    unmarked_note(marking_policy(&policies, &exe))
}

/// The QoS policies under `root`\\`path` (HKLM\\[`POLICY_KEY`]), each with
/// its string and DWORD values; none when the key is absent (no policy).
pub(super) fn read_policies(root: winreg::HKEY, path: &str) -> Vec<Policy> {
    let key = match winreg::Key::open(root, path) {
        Ok(key) => key,
        Err(ERROR_FILE_NOT_FOUND) => return Vec::new(),
        Err(code) => {
            debug!(code, path, "QoS policies unread: the key does not open");
            return Vec::new();
        }
    };
    let (names, error) = key.subkeys();
    if let Some(code) = error {
        debug!(
            code,
            path,
            read = names.len(),
            "QoS policies: listing stopped"
        );
    }
    names
        .into_iter()
        .filter_map(|name| match key.values(&name) {
            Ok(values) => Some(Policy {
                values: values
                    .into_iter()
                    .map(|(k, v)| {
                        let v = match v {
                            winreg::Value::Text(s) => PolicyValue::Text(s),
                            winreg::Value::Number(n) => PolicyValue::Number(n),
                        };
                        (k, v)
                    })
                    .collect(),
                name,
            }),
            Err(code) => {
                debug!(code, policy = %name, "QoS policy unread");
                None
            }
        })
        .collect()
}
