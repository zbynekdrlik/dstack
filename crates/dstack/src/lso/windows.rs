//! The NIC check's Windows glue: the adapter's settings and driver read
//! from the registry and the start line. Windows-only, so outside the
//! mutation gate (`.cargo/mutants.toml`); CI tests it on Windows. The
//! decisions are cross-platform ([`super::verdict`],
//! [`super::driver_verdict`], [`super::driver_line`],
//! [`super::standard::file_name`]).

use tracing::{info, warn};

use super::{DriverFacts, Verdict, driver_line, driver_verdict, standard, verdict};

/// The verdict for the adapter whose GUID is `adapter` (`{...}`).
pub fn check(adapter: &str) -> Verdict {
    match sys::lso_values(adapter) {
        Some((v2, v1)) => verdict(v2.as_deref(), v1.as_deref()),
        None => Verdict::Unknown,
    }
}

/// What the registry says of the driver of the adapter whose GUID is
/// `adapter`: its class subkey's `ProviderName`, `MatchingDeviceId`,
/// `DriverVersion` and `DriverDesc`, and the file of its service
/// (`Ndi\Service`, then the service's `ImagePath`); `None` when no class
/// subkey has the adapter.
pub fn driver_facts(adapter: &str) -> Option<DriverFacts> {
    let key = sys::adapter_key(adapter)?;
    let file = sys::read(&format!(r"{key}\Ndi"), "Service")
        .and_then(|service| sys::image_path(&service))
        .map(|path| standard::file_name(&path).to_string());
    Some(DriverFacts {
        provider: sys::read(&key, "ProviderName"),
        hardware_id: sys::read(&key, "MatchingDeviceId"),
        file,
        version: sys::read(&key, "DriverVersion"),
        description: sys::read(&key, "DriverDesc"),
    })
}

/// [`check`] and the driver verdict of `adapter`, logged: the start line's
/// companions (log-only glue, left out of the mutation gate).
pub fn report(adapter: &str) {
    match (driver_facts(adapter), standard::embedded()) {
        (Some(facts), Some(std)) => {
            if let Some(line) = driver_line(&driver_verdict(std, &facts), &facts) {
                if line.warn {
                    warn!("{}", line.text);
                } else {
                    info!("{}", line.text);
                }
            }
        }
        (None, _) => info!("the Dante NIC's driver was not read"),
        (Some(_), None) => {
            warn!("the embedded NIC standard does not parse: the Dante NIC's driver is not judged")
        }
    }
    match check(adapter) {
        Verdict::Off => info!("the Dante NIC's large send offload is off"),
        Verdict::On => warn!(
            "the Dante NIC has large send offload on: another sender's large sends delay our packets (late packets at a receiver); turn LSO off on this NIC (docs/windows-box.md)"
        ),
        Verdict::Unknown => info!("the Dante NIC's large send offload was not read"),
    }
}

pub(crate) mod sys {
    /// The network adapters' class key.
    const CLASS: &str =
        r"SYSTEM\CurrentControlSet\Control\Class\{4d36e972-e325-11ce-bfc1-08002be10318}";
    /// The services' key (a driver's `ImagePath`).
    const SERVICES: &str = r"SYSTEM\CurrentControlSet\Services";

    /// A string value of HKLM\`key`, `None` when absent.
    pub fn read(key: &str, value: &str) -> Option<String> {
        crate::winreg::read_sz(key, value)
    }

    /// The `ImagePath` of the service `service` (a REG_EXPAND_SZ value).
    pub fn image_path(service: &str) -> Option<String> {
        crate::winreg::read_text(&format!(r"{SERVICES}\{service}"), "ImagePath")
    }

    /// The `*LsoV2IPv4` and `*LsoV1IPv4` values of the adapter whose
    /// `NetCfgInstanceId` is `adapter`; `None` when no class subkey has it.
    pub fn lso_values(adapter: &str) -> Option<(Option<String>, Option<String>)> {
        let v = adapter_values(adapter, &["*LsoV2IPv4", "*LsoV1IPv4"])?;
        Some((v[0].clone(), v[1].clone()))
    }

    /// The class subkey (HKLM path) of the adapter whose `NetCfgInstanceId`
    /// is `adapter`; `None` when no class subkey has it.
    pub fn adapter_key(adapter: &str) -> Option<String> {
        (0..1000).find_map(|i| {
            let key = format!(r"{CLASS}\{i:04}");
            let id = read(&key, "NetCfgInstanceId")?;
            id.eq_ignore_ascii_case(adapter).then_some(key)
        })
    }

    /// The string values `names` of the adapter whose `NetCfgInstanceId`
    /// is `adapter`, in order; `None` when no class subkey has it.
    pub fn adapter_values(adapter: &str, names: &[&str]) -> Option<Vec<Option<String>>> {
        let key = adapter_key(adapter)?;
        Some(names.iter().map(|n| read(&key, n)).collect())
    }
}
