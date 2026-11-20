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
value of another profile. A Realtek NIC without an entry FAILs: a new chip is
measured and added before it carries production traffic. The exit status is 1
when anything FAILs, 2 when the standard cannot be read, else 0.

It only reads: no `ethtool -s/-K/-G/-C/-A/--set-eee`, nothing written.

    python3 -I nic-standard-check.py [--standard PATH|URL] [--profile normal|low_latency] [--all] [IFACE ...]

`--profile low_latency` adds the values only the lowest-latency hosts need
(iemmixer gen3); `--all` also checks NICs that are down; IFACEs limit the run
to those interfaces.
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
REALTEK_USB = "0bda"
REALTEK_PCI = "10ec"
STATUSES = ("settled", "open", "report")
COMPARES = ("equals", "bits", "contains")
PROFILES = ("normal", "low_latency")


class StandardError(Exception):
    """The standard file is missing, unreadable or not of this schema."""


# ------------------------------------------------------------ the standard


def validate(std):
    """Raises StandardError unless `std` is a well-formed standard: the
    checkers trust its shape, so a broken file stops the run instead of
    passing a PC by accident."""
    if not isinstance(std, dict) or std.get("schema") != SCHEMA:
        raise StandardError(f"not a {SCHEMA} file")
    if not isinstance(std.get("version"), int):
        raise StandardError("no integer version")
    seen = set()
    for entry in std.get("adapters", []):
        eid = entry.get("id")
        if not eid or eid in seen:
            raise StandardError(f"adapter id missing or repeated: {eid!r}")
        seen.add(eid)
        if entry.get("os") not in ("windows", "linux"):
            raise StandardError(f"{eid}: os must be windows or linux")
        if not entry.get("match"):
            raise StandardError(f"{eid}: no match")
        if not (entry.get("driver") or {}).get("evidence"):
            raise StandardError(f"{eid}: the driver has no evidence")
        for v in entry.get("values", []):
            key = v.get("key")
            where = f"{eid}/{key}"
            if not key or not v.get("evidence"):
                raise StandardError(f"{where}: a value needs a key and evidence")
            status = v.get("status", "settled")
            if status not in STATUSES:
                raise StandardError(f"{where}: unknown status {status!r}")
            if v.get("compare", "equals") not in COMPARES:
                raise StandardError(f"{where}: unknown compare")
            if v.get("absent", "fail") not in ("fail", "pass"):
                raise StandardError(f"{where}: absent must be fail or pass")
            if v.get("profile", "normal") not in PROFILES:
                raise StandardError(f"{where}: unknown profile")
            if (v.get("expected") is None) != (status == "report"):
                raise StandardError(f"{where}: expected is null exactly when status is report")
            if status == "open" and not v.get("plan"):
                raise StandardError(f"{where}: an open value needs its plan")
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


# ------------------------------------------------------------- judging


def as_list(expected):
    return expected if isinstance(expected, list) else [expected]


def holds(spec, actual):
    """Whether `actual` (a string) holds the value `spec` asks for."""
    compare = spec.get("compare", "equals")
    for want in as_list(spec["expected"]):
        if compare == "equals" and actual.strip() == str(want):
            return True
        if compare == "contains" and str(want) in actual:
            return True
        if compare == "bits":
            try:
                if int(actual.strip(), 0) & int(want, 0) == int(want, 0):
                    return True
            except ValueError:
                return False
    return False


def judge_value(spec, facts, profile):
    """One row (verdict, key, expected, actual, note) for a value."""
    key = spec["key"]
    expected = "|".join(str(e) for e in as_list(spec["expected"]))
    actual = facts.get(key)
    shown = "(absent)" if actual is None else actual
    note = spec["evidence"]
    if spec.get("profile", "normal") != "normal" and spec["profile"] != profile:
        return ("SKIP", key, expected, shown, f"{spec['profile']} profile only")
    status = spec.get("status", "settled")
    if status == "report":
        return ("INFO", key, "(no standard yet)", shown, note)
    if status == "open":
        note = "open: " + spec["plan"]
    if actual is None:
        verdict = "PASS" if spec.get("absent", "fail") == "pass" else "FAIL"
        return (verdict, key, expected, shown, note)
    return ("PASS" if holds(spec, actual) else "FAIL", key, expected, actual, note)


def version_tuple(version):
    return tuple(int(n) for n in re.findall(r"\d+", version))


def judge_driver(spec, facts):
    """The driver's rows: its file or module, its version, its firmware."""
    rows = []
    name = facts.get("driver", "")
    version = facts.get("driver:version", "")
    allowed = spec.get("files") or spec.get("modules") or []
    note = spec["evidence"]
    if spec.get("status") == "open" and spec.get("plan"):
        note = "open: " + spec["plan"]
    for bad in spec.get("bad", []):
        if bad.get("file", name) == name and bad.get("version", version) == version:
            rows.append(("FAIL", "driver", "not " + " ".join(filter(None, (bad.get("file"), bad.get("version")))), f"{name} {version}", bad["evidence"]))
            return rows
    if name not in allowed:
        rows.append(("FAIL", "driver", "|".join(allowed), name or "(none)", note))
        return rows
    rows.append(("PASS", "driver", "|".join(allowed), name, note))
    if spec.get("min_version"):
        ok = bool(version_tuple(version)) and version_tuple(version) >= version_tuple(spec["min_version"])
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


def format_row(iface, row):
    verdict, key, expected, actual, note = row
    return f"{verdict:<4}  {iface}  {key}  expected={expected}  actual={actual}  ({note})"


# ------------------------------------------------------------ this PC


def _read(path):
    try:
        with open(path, encoding="utf-8") as f:
            return f.read().strip()
    except OSError:
        return None


def nic_ids(iface, sysfs="/sys/class/net"):
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


def run(args):
    """A read-only command's stdout ('' when it fails: its values then read
    as absent, which fails unless the standard allows it)."""
    try:
        r = subprocess.run(args, capture_output=True, text=True, timeout=10, check=False)
    except (OSError, subprocess.TimeoutExpired) as e:
        print(f"INFO  {' '.join(args)} did not run: {e}", file=sys.stderr)
        return ""
    if r.returncode != 0:
        print(f"INFO  {' '.join(args)} exited {r.returncode}: {r.stderr.strip()}", file=sys.stderr)
    return r.stdout


def read_facts(iface, bus, sysfs="/sys/class/net"):
    facts = {}
    facts.update(parse_driver_info(run(["ethtool", "-i", iface])))
    facts.update(parse_features(run(["ethtool", "-k", iface])))
    facts.update(parse_ring(run(["ethtool", "-g", iface])))
    facts.update(parse_coalesce(run(["ethtool", "-c", iface])))
    facts.update(parse_pause(run(["ethtool", "-a", iface])))
    facts.update(parse_eee(run(["ethtool", "--show-eee", iface])))
    facts.update(parse_link(run(["ethtool", iface])))
    # The USB device's own power node is the interface's parent; the path
    # goes through the `device` symlink, so it is never normalized.
    parts = ["device", ".."] if bus == "usb" else ["device"]
    value = _read(os.path.join(sysfs, iface, *parts, "power", "control"))
    if value is not None:
        facts[f"{bus}:power/control"] = value
    quirks = _read("/sys/module/usbcore/parameters/quirks")
    if quirks is not None:
        facts["usbcore:quirks"] = quirks
    return facts


def main(argv=None):
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
    ifaces = a.ifaces or sorted(os.listdir("/sys/class/net"))
    failed = checked = 0
    for iface in ifaces:
        ids = nic_ids(iface)
        if ids is None or ids[1].split(":")[0] not in (REALTEK_USB, REALTEK_PCI):
            continue
        state = _read(f"/sys/class/net/{iface}/operstate")
        if state != "up" and not a.all and not a.ifaces:
            print(f"SKIP  {iface}  {ids[0]} {ids[1]} is {state}")
            continue
        checked += 1
        entry = find_entry(std, "linux", *ids)
        if entry is None:
            print(f"FAIL  {iface}  {ids[0]} {ids[1]}: no standard for this Realtek chip on Linux")
            failed += 1
            continue
        print(f"--    {iface}  {entry['chip']} ({entry['id']})")
        for row in check(entry, read_facts(iface, ids[0]), a.profile):
            print(format_row(iface, row))
            failed += row[0] == "FAIL"
    print(f"{checked} Realtek NIC(s) checked, {failed} FAIL")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
