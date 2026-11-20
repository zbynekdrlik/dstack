---
paths:
  - "docs/nic-standard.*"
  - "tools/windows/nic-standard-check.ps1"
  - "tools/windows/tests/nic-standard-check.Tests.ps1"
  - "tools/linux/nic-standard-check.py"
  - "tools/linux/tests/nic_standard_check_test.py"
---

# The Realtek NIC standard (docs/nic-standard.json, #465)

- **The file decides, nothing else.** A Realtek value or driver rule lives only in `docs/nic-standard.json`; docs and projects point at it. A change carries its evidence (ticket, measurement, vendor doc) in the same commit and raises `version` by one. Where nothing measured a value, keep the value the working PCs share and write `"evidence": "none yet: fleet value"`; never invent one. A value nobody agrees on and nobody measured is `"status": "report"` (printed, never fails); a value under measurement is `"status": "open"` with its `plan`. Both checkers validate the file first (Python `validate`, PowerShell `Test-NicStandard`, the same case-sensitive rules) and exit 2 on a malformed one: a value without evidence, an open one without a plan, a report one with an expected value, an unknown status/compare/absent/profile, a repeated key, a non-decimal bits value. CI runs both on the repo file.
- **Both checkers judge the same way.** The PowerShell one (Windows PCs have no Python) mirrors the Python one: same verdicts (PASS, FAIL, INFO, SKIP), `equals` and `contains` ignoring case, `bits` on decimal numbers only, versions padded with zeros (10.79.20 = 10.79.20.0), keywords matched without regard to case, evidence printed only on lines other than PASS. A rule change goes into both, with a test in each (`tests/nic-standard-check.Tests.ps1` on fake adapters, `tests/nic_standard_check_test.py` on real ethtool texts and a temporary sysfs tree). The Python judge works on Windows facts too, so a Windows PC's verdict can be computed from one inventory read without starting the checker there.
- **Unknown keys are refused** (both validators): a new field in the json is added to the key sets in both checkers first (`TOP_KEYS`/`ENTRY_KEYS`/`DRIVER_KEYS`/`BAD_KEYS`/`VALUE_KEYS`, `$script:NicKeys`). A Windows driver names `files`, a Linux one `modules`.
- **Named NICs** (`-Name` / `IFACE`) are checked up or not, and a missing or non-Realtek name fails: a PC with a second Realtek NIC that is no Dante leg (dsg's SoundGrid NIC) is checked by naming its Dante NIC.
- **PowerShell `return` unrolls a list:** a one-item JSON list comes back as its item, an empty one as nothing (`$null`). Read a list-valued property with `Get-NicList` and assign it (`$x = Get-NicList $o 'files'`); `@(...)` around a read is fine where only the items matter.
- **Windows reads:** `Get-NetAdapterAdvancedProperty -AllProperties` returns the hidden registry keywords too (the 11.19 RTL8156B driver does not offer the power-saving keywords; songplayer writes them anyway). Keyword case differs between drivers (`*PriorityVlanTag` on rt640x64, `*PriorityVLANTag` on the USB drivers): match case-insensitively. The chip comes from the first hardware id (`DEVPKEY_Device_HardwareIds`); the VLAN adapter's is `RTK_VLANMP`.
- **RTL8156 or RTL8156B:** one USB id (`0bda:8156`). The revision tells them apart: bcdDevice / `REV_` 31xx is the B (Realtek's 1156.22 INF maps REV_31F4 to its `RTL8156Bx64` section; the in-tree r8152 loads `rtl8156b-2` firmware). Every one in the fleet (20.11.2026) is a B.
- **Linux power nodes:** a USB NIC's own node is `/sys/class/net/<if>/device/../power/control` (the interface's parent is the USB device); a PCI NIC's is `device/power/control`: `device/..` is the PCI bridge, which reads `auto` while the NIC reads `on`. Never normalize these paths (`device` is a symlink).
- **r8152's `rx-usecs`** reads 15000 on the in-tree driver (ns) and 15 on Realtek's v2.21.4 (us): the same default. Never `ethtool -C` a running r8152 link (it restarts the MAC, #290).
- **Running a checker on a PC:** fetch the script and the json by commit SHA from raw.githubusercontent into a temp dir, run, remove the dir. On dsg and FOH (production audio) keep to one read: every new `powershell.exe` with the NetAdapter module costs ~2.4 s wall and ~1.6 s CPU (#312).
- **A hyphenated Python tool** (`nic-standard-check.py`) is imported by its test with `importlib.util.spec_from_file_location`.
