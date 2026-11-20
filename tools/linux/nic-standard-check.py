#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Checks a Linux PC's Realtek NICs against dstack's NIC standard (dstack #465).

The standard is `docs/nic-standard.json` (by path or URL; the default is the
file on dstack's main branch, a project pins a tag's copy instead). For every
Realtek NIC that is up, it finds the standard's entry for the chip (by its USB
or PCI id), reads the driver and the values with `ethtool` and sysfs, and
prints one line per value:

    PASS  rig0  ring:rx  expected=4096  actual=4096
    FAIL  rig0  pause:tx  expected=off  actual=on  (dstack #425)

PASS and FAIL judge a value; INFO prints a value the standard does not decide
yet (status `report`, or a driver version under an open question); SKIP is a
value of another profile or a NIC that is down. A Realtek NIC without an entry
FAILs: a new chip is measured and added before it carries production traffic.
So does an interface named on the command line that is missing or no Realtek.

First, whatever the vendor, it checks the standard's vendor-independent rule
`eee` on EVERY wired physical NIC, up or down, with or without an entry and
whatever IFACEs name (the owner, 20.11.2026: EEE enabled on any rig device is
a serious failure): `ethtool --show-eee` "enabled - active" and "enabled -
inactive" FAIL, "disabled" and "not supported" pass. A FAIL names the NIC,
its driver and the key:

    FAIL  enp1s0  eee:status  expected=disabled|not supported  actual=enabled - inactive  (driver r8169; ...)

The exit status is 1 when anything FAILs, 2 when the standard cannot be read
or is malformed, else 0.

It only reads: no `ethtool -s/-K/-G/-C/-A/--set-eee`, nothing written.

    python3 -I nic-standard-check.py [--standard PATH|URL] [--profile normal|low_latency] [--all] [IFACE ...]

`--profile low_latency` adds the values only the lowest-latency hosts need
(iemmixer gen3, dasio); `--all` also checks NICs that are down; IFACEs limit
the run to those interfaces.

The judging (validate, holds, judge_value, judge_driver) is the same as the
PowerShell checker's (tools/windows/nic-standard-check.ps1), and works on
Windows facts too (registry keywords, matched without regard to case).
"""

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.request

SCHEMA = "dstack-nic-standard/1"
DEFAULT_STANDARD = (
    "https://raw.githubusercontent.com/zbynekdrlik/dstack/main/docs/nic-standard.json"
)
SYSFS_NET = "/sys/class/net"
USBCORE_QUIRKS = "/sys/module/usbcore/parameters/quirks"
REALTEK_USB = "0bda"
REALTEK_PCI = "10ec"
STATUSES = ("settled", "open", "report")
COMPARES = ("equals", "bits", "contains")
PROFILES = ("normal", "low_latency")
MATCH_KEYS = {"windows": ("hardware_id",), "linux": ("usb", "pci")}


class StandardError(Exception):
    """The standard file is missing, unreadable or malformed."""


# ------------------------------------------------------------ the standard


def as_list(expected):
    return expected if isinstance(expected, list) else [expected]


def _strings(value):
    """A non-empty list of non-empty strings."""
    return isinstance(value, list) and bool(value) and all(isinstance(x, str) and x for x in value)


def is_number(text):
    """A decimal number of at most 18 digits (ASCII only: no other digit
    characters, no sign, no space), so it always fits a 64-bit integer."""
    return isinstance(text, str) and re.fullmatch(r"[0-9]{1,18}", text) is not None


# The keys each object may hold: an unknown key is a typo, never ignored.
TOP_KEYS = {"schema", "version", "updated", "issue", "doc", "scope", "fields", "eee", "adapters"}
EEE_KEYS = {"name", "scope", "windows", "linux", "evidence"}
EEE_WINDOWS_KEYS = {"keywords", "off", "display_name", "display_off", "display_on"}
EEE_LINUX_KEYS = {"off"}
ENTRY_KEYS = {"id", "chip", "covers", "os", "match", "driver", "values"}
DRIVER_KEYS = {"files", "modules", "min_version", "bad", "status", "evidence", "plan", "firmware"}
BAD_KEYS = {"file", "version", "evidence"}
VALUE_KEYS = {"key", "name", "expected", "compare", "absent", "status", "profile", "evidence", "plan"}
# The driver's names per OS: Windows judges the driver file, Linux the module.
DRIVER_NAMES = {"windows": "files", "linux": "modules"}


def _only(where, obj, allowed):
    unknown = sorted(set(obj) - allowed)
    if unknown:
        raise StandardError(f"{where}: unknown key(s) {', '.join(unknown)}")


def _text(where, obj, key, required=False):
    """A string field: absent unless required, else a non-empty string."""
    if key not in obj:
        if required:
            raise StandardError(f"{where}: no {key}")
        return
    if not isinstance(obj[key], str) or not obj[key]:
        raise StandardError(f"{where}: {key} must be a non-empty string")


def _validate_driver(eid, os_name, driver):
    if not isinstance(driver, dict):
        raise StandardError(f"{eid}: no driver")
    _only(f"{eid}/driver", driver, DRIVER_KEYS)
    _text(f"{eid}/driver", driver, "evidence", required=True)
    names = DRIVER_NAMES[os_name]
    other = DRIVER_NAMES["linux" if os_name == "windows" else "windows"]
    if not _strings(driver.get(names)) or other in driver:
        raise StandardError(f"{eid}: a {os_name} driver names its {names} (and no {other})")
    for key in ("min_version", "plan", "firmware"):
        _text(f"{eid}/driver", driver, key)
    if os_name == "windows" and "firmware" in driver:
        raise StandardError(f"{eid}: Windows has no firmware read; a firmware rule is Linux only")
    status = driver.get("status", "settled")
    if status not in ("settled", "open"):
        raise StandardError(f"{eid}: unknown driver status {status!r}")
    if status == "open" and "plan" not in driver:
        raise StandardError(f"{eid}: an open driver rule needs its plan")
    bads = driver.get("bad", [])
    if not isinstance(bads, list):
        raise StandardError(f"{eid}: bad must be a list")
    for bad in bads:
        if not isinstance(bad, dict):
            raise StandardError(f"{eid}: a bad driver is an object")
        _only(f"{eid}/bad", bad, BAD_KEYS)
        _text(f"{eid}/bad", bad, "evidence", required=True)
        _text(f"{eid}/bad", bad, "file")
        _text(f"{eid}/bad", bad, "version")
        if "file" not in bad and "version" not in bad:
            raise StandardError(f"{eid}: a bad driver names a file or a version")


def _validate_value(eid, v, keys):
    key = v.get("key") if isinstance(v, dict) else None
    where = f"{eid}/{key}"
    if not isinstance(key, str) or not key:
        raise StandardError(f"{where}: a value needs a key")
    _only(where, v, VALUE_KEYS)
    _text(where, v, "evidence", required=True)
    _text(where, v, "name")
    _text(where, v, "plan")
    if key.lower() in keys:
        raise StandardError(f"{where}: the key is repeated")
    keys.add(key.lower())
    status = v.get("status", "settled")
    if status not in STATUSES:
        raise StandardError(f"{where}: unknown status {status!r}")
    if v.get("compare", "equals") not in COMPARES:
        raise StandardError(f"{where}: unknown compare")
    if v.get("absent", "fail") not in ("fail", "pass"):
        raise StandardError(f"{where}: absent must be fail or pass")
    if v.get("profile", "normal") not in PROFILES:
        raise StandardError(f"{where}: unknown profile")
    expected = v.get("expected")
    if status == "report":
        if expected is not None:
            raise StandardError(f"{where}: a report value has no expected value")
        return
    if not (isinstance(expected, str) and expected) and not _strings(expected):
        raise StandardError(f"{where}: expected must be a string or a list of strings")
    if v.get("compare") == "bits" and not all(is_number(x) for x in as_list(expected)):
        raise StandardError(f"{where}: a bits value is a decimal number")
    if status == "open" and "plan" not in v:
        raise StandardError(f"{where}: an open value needs its plan")


def _validate_eee(rule):
    """The vendor-independent EEE rule: Windows keywords and display
    patterns, Linux passing states (dstack #465, owner 20.11.2026)."""
    if not isinstance(rule, dict):
        raise StandardError("no eee rule")
    _only("eee", rule, EEE_KEYS)
    _text("eee", rule, "evidence", required=True)
    _text("eee", rule, "name")
    _text("eee", rule, "scope")
    win, lin = rule.get("windows"), rule.get("linux")
    if not isinstance(win, dict) or not isinstance(lin, dict):
        raise StandardError("eee: needs its windows and linux parts")
    _only("eee/windows", win, EEE_WINDOWS_KEYS)
    _only("eee/linux", lin, EEE_LINUX_KEYS)
    for where, obj, key in (("eee/windows", win, "keywords"), ("eee/windows", win, "off"), ("eee/linux", lin, "off")):
        if not _strings(obj.get(key)):
            raise StandardError(f"{where}: {key} must be a list of strings")
    for key in ("display_name", "display_off", "display_on"):
        _text("eee/windows", win, key, required=True)
        try:
            re.compile(win[key])
        except re.error as e:
            raise StandardError(f"eee/windows: {key} is no regular expression: {e}") from e


def validate(std):
    """Raises StandardError unless `std` is a well-formed standard: the
    checkers trust its shape, so a broken file stops the run (exit 2)
    instead of passing a PC by accident. Every rule is case-sensitive, as
    the PowerShell checker's `Test-NicStandard`."""
    if not isinstance(std, dict) or std.get("schema") != SCHEMA:
        raise StandardError(f"not a {SCHEMA} file")
    _only("the file", std, TOP_KEYS)
    if not isinstance(std.get("version"), int) or isinstance(std.get("version"), bool):
        raise StandardError("no integer version")
    _validate_eee(std.get("eee"))
    if not isinstance(std.get("adapters"), list):
        raise StandardError("no adapters list")
    seen = set()
    for entry in std["adapters"]:
        eid = entry.get("id") if isinstance(entry, dict) else None
        if not isinstance(eid, str) or not eid or eid in seen:
            raise StandardError(f"adapter id missing or repeated: {eid!r}")
        seen.add(eid)
        _only(eid, entry, ENTRY_KEYS)
        os_name = entry.get("os")
        if os_name not in MATCH_KEYS:
            raise StandardError(f"{eid}: os must be windows or linux")
        match = entry.get("match")
        if not isinstance(match, dict) or not match or any(
            k not in MATCH_KEYS[os_name] or not _strings(ids) for k, ids in match.items()
        ):
            raise StandardError(f"{eid}: match needs {' or '.join(MATCH_KEYS[os_name])} id lists")
        _validate_driver(eid, os_name, entry.get("driver"))
        if not isinstance(entry.get("values"), list):
            raise StandardError(f"{eid}: no values list")
        keys = set()
        for v in entry["values"]:
            _validate_value(eid, v, keys)
    return std


def load_standard(source):
    """The standard from a path or an http(s) URL, validated."""
    try:
        if re.match(r"https?://", source):
            with urllib.request.urlopen(source, timeout=20) as r:
                text = r.read().decode("utf-8")
        else:
            with open(source, encoding="utf-8") as f:
                text = f.read()
        std = json.loads(text)
    except (OSError, ValueError) as e:
        raise StandardError(f"cannot read the standard {source}: {e}") from e
    return validate(std)


def find_entry(std, os_name, bus, ident):
    """The entry for a NIC (`bus` usb or pci, `ident` vendor:product in
    lowercase hex), or None."""
    for entry in std["adapters"]:
        if entry["os"] == os_name and ident in entry["match"].get(bus, []):
            return entry
    return None


# ------------------------------------------------------- ethtool's output


def _colon_pairs(text):
    out = {}
    for line in text.splitlines():
        if ":" in line:
            k, v = line.split(":", 1)
            out[k.strip()] = v.strip()
    return out


def parse_driver_info(text):
    """`ethtool -i`: driver, version, firmware."""
    p = _colon_pairs(text)
    return {
        "driver": p.get("driver", ""),
        "driver:version": p.get("version", ""),
        "firmware": p.get("firmware-version", ""),
    }


def parse_features(text):
    """`ethtool -k`: every feature as `feature:<name>` -> on/off."""
    out = {}
    for line in text.splitlines():
        if ":" not in line or line.startswith("Features for"):
            continue
        k, v = line.split(":", 1)
        words = v.split()
        if words:
            out["feature:" + k.strip()] = words[0]
    return out


def parse_ring(text):
    """`ethtool -g`: the current RX and TX ring sizes (n/a dropped)."""
    out = {}
    current = False
    for line in text.splitlines():
        if line.startswith("Current hardware settings"):
            current = True
            continue
        if current and ":" in line:
            k, v = line.split(":", 1)
            k, v = k.strip(), v.strip()
            if k in ("RX", "TX") and v != "n/a":
                out["ring:" + k.lower()] = v
    return out


def parse_coalesce(text):
    """`ethtool -c`: the usecs values (n/a dropped)."""
    out = {}
    for k, v in _colon_pairs(text).items():
        if k in ("rx-usecs", "tx-usecs") and v != "n/a":
            out["coalesce:" + k] = v
    return out


def parse_pause(text):
    """`ethtool -a`: autonegotiate, rx and tx pause."""
    p = _colon_pairs(text)
    names = {"Autonegotiate": "autonegotiate", "RX": "rx", "TX": "tx"}
    return {"pause:" + names[k]: v for k, v in p.items() if k in names}


def parse_eee(text):
    """`ethtool --show-eee`: the status as disabled, enabled or not supported
    ("enabled - inactive" is enabled: it applies once the partner offers it)."""
    m = re.search(r"EEE status:\s*(.+)", text)
    if not m:
        return {}
    status = m.group(1).strip()
    if status.startswith("enabled"):
        status = "enabled"
    return {"eee:status": status}


def parse_link(text):
    """`ethtool <iface>`: auto-negotiation, speed and duplex."""
    p = _colon_pairs(text)
    out = {}
    for name, key in (("Auto-negotiation", "autoneg"), ("Speed", "speed"), ("Duplex", "duplex")):
        if name in p:
            out["link:" + key] = p[name]
    return out


def parse_no_lpm(quirks, ident):
    """Whether usbcore's `quirks` parameter (`vid:pid:flags,...`) gives the
    USB device `ident` the NO_LPM flag `k`: `usbcore:no_lpm` on/off."""
    for item in quirks.split(","):
        parts = item.strip().lower().split(":")
        if len(parts) == 3 and f"{parts[0]}:{parts[1]}" == ident and "k" in parts[2]:
            return {"usbcore:no_lpm": "on"}
    return {"usbcore:no_lpm": "off"}


# ------------------------------------------------------------- judging


def holds(spec, actual):
    """Whether `actual` (a string) holds the value `spec` asks for: equals
    and contains ignore case, bits takes decimal numbers."""
    compare = spec.get("compare", "equals")
    value = actual.strip()
    for want in as_list(spec["expected"]):
        if compare == "equals" and value.casefold() == want.casefold():
            return True
        if compare == "contains" and want.casefold() in value.casefold():
            return True
        if compare == "bits":
            if not is_number(value):
                return False
            if int(value) & int(want) == int(want):
                return True
    return False


def _lookup(facts, key):
    """A fact by key, its case ignored (registry keywords differ in case
    between drivers: `*PriorityVlanTag`, `*PriorityVLANTag`)."""
    if key in facts:
        return facts[key]
    folded = key.casefold()
    for k, v in facts.items():
        if k.casefold() == folded:
            return v
    return None


def judge_value(spec, facts, profile):
    """One row (verdict, key, expected, actual, note) for a value."""
    key = spec["key"]
    status = spec.get("status", "settled")
    expected = "(no standard yet)" if status == "report" else "|".join(as_list(spec["expected"]))
    actual = _lookup(facts, key)
    shown = "(absent)" if actual is None else actual
    note = spec["evidence"]
    if spec.get("profile", "normal") != "normal" and spec["profile"] != profile:
        return ("SKIP", key, expected, shown, f"{spec['profile']} profile only")
    if status == "report":
        return ("INFO", key, expected, shown, note)
    if status == "open":
        note = "open: " + spec["plan"]
    if actual is None:
        verdict = "PASS" if spec.get("absent", "fail") == "pass" else "FAIL"
        return (verdict, key, expected, shown, note)
    return ("PASS" if holds(spec, actual) else "FAIL", key, expected, actual, note)


def compare_versions(a, b):
    """-1, 0 or 1: dotted versions compared number by number, the shorter
    padded with zeros (10.79.20 equals 10.79.20.0)."""
    x = [int(n) for n in re.findall(r"\d+", a)]
    y = [int(n) for n in re.findall(r"\d+", b)]
    width = max(len(x), len(y))
    x += [0] * (width - len(x))
    y += [0] * (width - len(y))
    return (x > y) - (x < y)


def judge_driver(spec, facts):
    """The driver's rows: a bad driver, an allowed file or module, the
    minimum version, the firmware."""
    rows = []
    name = facts.get("driver", "")
    version = facts.get("driver:version", "")
    allowed = spec.get("files") or spec.get("modules") or []
    note = spec["evidence"]
    if spec.get("status") == "open":
        note = "open: " + spec["plan"]
    for bad in spec.get("bad", []):
        if bad.get("file", name).casefold() == name.casefold() and bad.get("version", version) == version:
            what = " ".join(filter(None, (bad.get("file"), bad.get("version"))))
            return [("FAIL", "driver", "not " + what, f"{name} {version}".strip(), bad["evidence"])]
    if name.casefold() not in [a.casefold() for a in allowed]:
        return [("FAIL", "driver", "|".join(allowed), name or "(none)", note)]
    rows.append(("PASS", "driver", "|".join(allowed), name, note))
    if spec.get("min_version"):
        ok = bool(version) and compare_versions(version, spec["min_version"]) >= 0
        rows.append(("PASS" if ok else "FAIL", "driver:version", ">=" + spec["min_version"], version or "(none)", note))
    else:
        rows.append(("INFO", "driver:version", "(no standard yet)", version or "(none)", note))
    if "firmware" in spec:
        fw = facts.get("firmware", "")
        rows.append(("PASS" if fw == spec["firmware"] else "FAIL", "firmware", spec["firmware"], fw or "(none)", note))
    return rows


def check(entry, facts, profile):
    """Every row for one NIC against its entry."""
    rows = judge_driver(entry["driver"], facts)
    rows += [judge_value(v, facts, profile) for v in entry["values"]]
    return rows


# ------------------------------------------------ EEE on every wired NIC


def _eee_note(rule, driver):
    """A FAIL line names the NIC (its own column), its driver and the rule's
    evidence."""
    return f"driver {driver or '(unknown)'}; {rule['evidence']}"


# ethtool's own answer for a driver without EEE support (ixgbe): new ethtool
# over netlink, old over ioctl. Only this answer, and only with no status.
EEE_UNSUPPORTED = re.compile(
    r"^(netlink error|Cannot get EEE settings): Operation not supported", re.MULTILINE
)


def judge_eee_linux(rule, text, driver):
    """The EEE row of a Linux NIC from `ethtool --show-eee` (stdout and
    stderr together): "enabled - active" and "enabled - inactive" are
    enabled (the actual value shows which); ethtool's answer for a driver
    without EEE support is not supported; no answer is unreadable, a FAIL."""
    off = rule["linux"]["off"]
    m = re.search(r"EEE status:\s*(.+)", text)
    shown = m.group(1).strip() if m else None
    status = parse_eee(text).get("eee:status")
    if shown is None and EEE_UNSUPPORTED.search(text):
        shown = status = "not supported"
    expected = "|".join(off)
    note = _eee_note(rule, driver)
    if status is None:
        return ("FAIL", "eee:status", expected, "(unreadable)", note)
    ok = status.casefold() in [o.casefold() for o in off]
    return ("PASS" if ok else "FAIL", "eee:status", expected, shown, note)


def judge_eee_windows(rule, props, driver):
    """The EEE rows of a Windows NIC from its advanced properties, each
    (keyword, value, display name, display value), the hidden ones (no
    display name) included. A listed keyword (any case) passes when its
    value is off. Another keyword whose display name matches is judged by
    its display value: off passes, on fails; with no display value its
    value decides; anything else (Realtek's EEEMaxSupportSpeed, a speed) is
    INFO. A NIC with no EEE keyword at all gives one INFO row: the
    driver's default holds and Windows shows no state for it."""
    win = rule["windows"]
    keywords = {k.casefold() for k in win["keywords"]}
    off = [o.casefold() for o in win["off"]]
    note = _eee_note(rule, driver)
    rows = []
    for keyword, value, display_name, display_value in props:
        value = "" if value is None else str(value).strip()
        display_value = (display_value or "").strip()
        shown = f"{value} ({display_value})" if display_value else value
        listed = keyword.casefold() in keywords
        if listed and value != "":
            rows.append(("PASS" if value.casefold() in off else "FAIL", keyword, "|".join(win["off"]), shown, note))
        elif (listed and display_value) or (
            not listed and display_name and re.search(win["display_name"], display_name, re.IGNORECASE)
        ):
            # A listed keyword with no registry value but a display value
            # (the driver's default shown) is judged as an unlisted one.
            if display_value and re.search(win["display_off"], display_value, re.IGNORECASE):
                verdict = "PASS"
            elif display_value and re.search(win["display_on"], display_value, re.IGNORECASE):
                verdict = "FAIL"
            elif not display_value and value.casefold() in off:
                verdict = "PASS"
            else:
                verdict = "INFO"
            rows.append((verdict, keyword, "off", shown, note))
    if not rows:
        rows.append(("INFO", "eee", "(a keyword)", "(no EEE keyword)", "the driver's default holds, unread; " + note))
    return rows


def format_row(iface, row):
    """One output line; the evidence or plan only where the reader acts on
    it (FAIL, INFO, SKIP), so a clean run stays short."""
    verdict, key, expected, actual, note = row
    line = f"{verdict:<4}  {iface}  {key}  expected={expected}  actual={actual}"
    return line if verdict == "PASS" else f"{line}  ({note})"


# ------------------------------------------------------------ this PC


def _read(path):
    try:
        with open(path, encoding="utf-8") as f:
            return f.read().strip()
    except OSError:
        return None


def nic_ids(iface, sysfs=SYSFS_NET):
    """(bus, vendor:product) of a NIC, or None for a virtual interface."""
    dev = os.path.join(sysfs, iface, "device")
    if not os.path.exists(dev):
        return None
    vendor = _read(os.path.join(dev, "vendor"))
    device = _read(os.path.join(dev, "device"))
    if vendor and device:
        return ("pci", f"{vendor.lower().replace('0x', '')}:{device.lower().replace('0x', '')}")
    usb = os.path.join(dev, "..")
    vendor = _read(os.path.join(usb, "idVendor"))
    product = _read(os.path.join(usb, "idProduct"))
    if vendor and product:
        return ("usb", f"{vendor.lower()}:{product.lower()}")
    return None


def run(args, with_stderr=False):
    """A read-only command's stdout ('' when it fails: its values then read
    as absent, which fails unless the standard allows it). `with_stderr`
    adds its stderr to the text (ethtool reports an unsupported EEE there)."""
    try:
        r = subprocess.run(args, capture_output=True, text=True, timeout=10, check=False)
    except (OSError, subprocess.TimeoutExpired) as e:
        print(f"INFO  {' '.join(args)} did not run: {e}", file=sys.stderr)
        return ""
    if with_stderr:
        return r.stdout + r.stderr
    if r.returncode != 0:
        print(f"INFO  {' '.join(args)} exited {r.returncode}: {r.stderr.strip()}", file=sys.stderr)
    return r.stdout


def wired_nics(sysfs=SYSFS_NET):
    """Every wired physical NIC: an interface with a device, no wireless
    one, of type 1 (Ethernet) where the type is readable."""
    out = []
    for iface in sorted(os.listdir(sysfs)):
        base = os.path.join(sysfs, iface)
        if not os.path.exists(os.path.join(base, "device")):
            continue
        if os.path.exists(os.path.join(base, "wireless")) or os.path.exists(os.path.join(base, "phy80211")):
            continue
        kind = _read(os.path.join(base, "type"))
        if kind is not None and kind != "1":
            continue
        out.append(iface)
    return out


def nic_driver(iface, sysfs=SYSFS_NET):
    """The kernel driver bound to the NIC's device (sysfs), or ''."""
    link = os.path.join(sysfs, iface, "device", "driver")
    return os.path.basename(os.path.realpath(link)) if os.path.exists(link) else ""


def check_eee(rule, sysfs=SYSFS_NET):
    """The EEE row of every wired NIC of this PC, up or down, any vendor."""
    return [
        (iface, judge_eee_linux(rule, run(["ethtool", "--show-eee", iface], with_stderr=True), nic_driver(iface, sysfs)))
        for iface in wired_nics(sysfs)
    ]


def power_node(iface, bus, sysfs=SYSFS_NET):
    """The NIC's own runtime-PM node: a USB NIC's is its USB device's (the
    interface's parent), a PCI NIC's its own (`device/..` would be the
    bridge). Never normalized: `device` is a symlink."""
    parts = ["device", ".."] if bus == "usb" else ["device"]
    return os.path.join(sysfs, iface, *parts, "power", "control")


def read_facts(iface, ids, sysfs=SYSFS_NET, quirks_path=USBCORE_QUIRKS):
    bus, ident = ids
    facts = {}
    facts.update(parse_driver_info(run(["ethtool", "-i", iface])))
    facts.update(parse_features(run(["ethtool", "-k", iface])))
    facts.update(parse_ring(run(["ethtool", "-g", iface])))
    facts.update(parse_coalesce(run(["ethtool", "-c", iface])))
    facts.update(parse_pause(run(["ethtool", "-a", iface])))
    facts.update(parse_eee(run(["ethtool", "--show-eee", iface])))
    facts.update(parse_link(run(["ethtool", iface])))
    value = _read(power_node(iface, bus, sysfs))
    if value is not None:
        facts[f"{bus}:power/control"] = value
    if bus == "usb":
        quirks = _read(quirks_path)
        if quirks is not None:
            facts.update(parse_no_lpm(quirks, ident))
    return facts


def main(argv=None, sysfs=SYSFS_NET, quirks_path=USBCORE_QUIRKS):
    ap = argparse.ArgumentParser(description="Check this PC's Realtek NICs against dstack's NIC standard (read-only).")
    ap.add_argument("--standard", default=DEFAULT_STANDARD, help="the standard's path or URL")
    ap.add_argument("--profile", choices=PROFILES, default="normal")
    ap.add_argument("--all", action="store_true", help="also check NICs that are down")
    ap.add_argument("ifaces", nargs="*")
    a = ap.parse_args(argv)
    try:
        std = load_standard(a.standard)
    except StandardError as e:
        print(f"FAIL  {e}")
        return 2
    print(f"nic-standard v{std['version']} ({a.standard}), profile {a.profile}")
    # EEE first: every wired NIC of the PC, whatever IFACEs name.
    print(f"--    every wired NIC: {std['eee'].get('name', 'EEE off')} (eee)")
    eee_rows = check_eee(std["eee"], sysfs)
    for iface, row in eee_rows:
        print(format_row(iface, row))
    eee_failed = sum(row[0] == "FAIL" for _, row in eee_rows)
    print(f"{len(eee_rows)} wired NIC(s) checked for EEE, {eee_failed} FAIL")
    named = bool(a.ifaces)
    ifaces = a.ifaces or sorted(os.listdir(sysfs))
    failed = checked = 0
    for iface in ifaces:
        ids = nic_ids(iface, sysfs)
        if ids is None or ids[1].split(":")[0] not in (REALTEK_USB, REALTEK_PCI):
            if named:
                print(f"FAIL  {iface}: no such Realtek NIC")
                failed += 1
            continue
        state = _read(os.path.join(sysfs, iface, "operstate"))
        if state != "up" and not a.all and not named:
            print(f"SKIP  {iface}  {ids[0]} {ids[1]} is {state}")
            continue
        checked += 1
        entry = find_entry(std, "linux", *ids)
        if entry is None:
            print(f"FAIL  {iface}  {ids[0]} {ids[1]}: no standard for this Realtek chip on Linux")
            failed += 1
            continue
        print(f"--    {iface}  {entry['chip']} ({entry['id']})")
        for row in check(entry, read_facts(iface, ids, sysfs, quirks_path), a.profile):
            print(format_row(iface, row))
            failed += row[0] == "FAIL"
    print(f"{checked} Realtek NIC(s) checked, {failed} FAIL")
    return 1 if failed or eee_failed else 0


if __name__ == "__main__":
    sys.exit(main())
