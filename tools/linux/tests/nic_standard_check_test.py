#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests of the NIC standard and its Linux checker (dstack #465):
`python3 -m unittest tools/linux/tests/nic_standard_check_test.py`.

The ethtool texts below are the real outputs read on the fleet on 20.11.2026
(stream-snv's USB RTL8156B on r8152 v2.21.4, the iemmixer box's RTL8168H on
r8169), trimmed to the lines the checker reads."""

import contextlib
import copy
import importlib.util
import io
import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", "..", ".."))
STANDARD = os.path.join(ROOT, "docs", "nic-standard.json")

_spec = importlib.util.spec_from_file_location(
    "nic_standard_check", os.path.join(HERE, "..", "nic-standard-check.py")
)
nsc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(nsc)

STREAM_I = """driver: r8152
version: v2.21.4 (2025/10/28)
firmware-version:
bus-info: usb-0000:00:14.0-4
"""
STREAM_K = """Features for rig0:
rx-checksumming: on
tx-checksumming: on
\ttx-checksum-ipv4: on
scatter-gather: on
tcp-segmentation-offload: on
generic-segmentation-offload: on
generic-receive-offload: on
rx-vlan-offload: on
tx-vlan-offload: on
tx-nocache-copy: off
hw-tc-offload: off [fixed]
"""
STREAM_C = """Coalesce parameters for rig0:
Adaptive RX: n/a  TX: n/a
rx-usecs:\t15
rx-frames:\tn/a
tx-usecs:\t0
"""
STREAM_G = """Ring parameters for rig0:
Pre-set maximums:
RX:\t\t\t4096
TX:\t\t\tn/a
Current hardware settings:
RX:\t\t\t4096
TX:\t\t\tn/a
"""
STREAM_A = """Pause parameters for rig0:
Autonegotiate:\ton
RX:\t\toff
TX:\t\toff
"""
STREAM_EEE = """EEE settings for rig0:
\tEEE status: disabled
\tTx LPI: disabled
"""
STREAM_LINK = """Settings for rig0:
\tSpeed: 2500Mb/s
\tDuplex: Full
\tAuto-negotiation: on
\tLink detected: yes
"""
IEM_EEE = """EEE settings for enp2s0:
\tEEE status: enabled - inactive
\tTx LPI: 12 (us)
"""
COMPANION_EEE = """EEE settings for enp2s0:
\tEEE status: not supported
"""
STREAM_OUTPUTS = {
    "-i": STREAM_I, "-k": STREAM_K, "-c": STREAM_C, "-g": STREAM_G,
    "-a": STREAM_A, "--show-eee": STREAM_EEE, "": STREAM_LINK,
}
# A cable-less NIC (the PP editing PC's unused onboard enp6s0 and enp7s0,
# 20.11.2026): ethtool reads no speed or duplex.
NO_LINK = """Settings for enp6s0:
\tSpeed: Unknown!
\tDuplex: Unknown! (255)
\tAuto-negotiation: on
\tLink detected: no
"""
# The PP editing PC's RTL8125 (enp6s0, r8169, no cable), 20.11.2026: the one
# Linux RTL8125 in the fleet. ethtool -c answers "Operation not supported".
RTL8125_I = """driver: r8169
version: 7.0.0-38-generic
firmware-version: rtl8125b-2_0.0.2 07/13/20
bus-info: 0000:06:00.0
"""
RTL8125_K = """Features for enp6s0:
rx-checksumming: on
tx-checksumming: on
scatter-gather: on
tcp-segmentation-offload: on
generic-segmentation-offload: on
generic-receive-offload: on
large-receive-offload: off [fixed]
rx-vlan-offload: on
tx-vlan-offload: on
"""
RTL8125_G = """Ring parameters for enp6s0:
Pre-set maximums:
RX:\t\t\t256
TX:\t\t\t256
Current hardware settings:
RX:\t\t\t256
TX:\t\t\t256
"""
RTL8125_A = """Pause parameters for enp6s0:
Autonegotiate:\ton
RX:\t\toff
TX:\t\toff
"""
RTL8125_EEE = """EEE settings for enp6s0:
\tEEE status: disabled
\tTx LPI: 0 (us)
"""


def rtl8125_facts():
    facts = {}
    facts.update(nsc.parse_driver_info(RTL8125_I))
    facts.update(nsc.parse_features(RTL8125_K))
    facts.update(nsc.parse_ring(RTL8125_G))
    facts.update(nsc.parse_coalesce("netlink error: Operation not supported\n"))
    facts.update(nsc.parse_pause(RTL8125_A))
    facts.update(nsc.parse_eee(RTL8125_EEE))
    facts.update(nsc.parse_link(NO_LINK))
    facts["pci:power/control"] = "on"
    return facts


def stream_facts():
    facts = {}
    facts.update(nsc.parse_driver_info(STREAM_I))
    facts.update(nsc.parse_features(STREAM_K))
    facts.update(nsc.parse_ring(STREAM_G))
    facts.update(nsc.parse_coalesce(STREAM_C))
    facts.update(nsc.parse_pause(STREAM_A))
    facts.update(nsc.parse_eee(STREAM_EEE))
    facts.update(nsc.parse_link(STREAM_LINK))
    facts["usb:power/control"] = "on"
    facts.update(nsc.parse_no_lpm("0bda:8156:k", "0bda:8156"))
    return facts


def load():
    return nsc.load_standard(STANDARD)


def entry(std, eid):
    return next(e for e in std["adapters"] if e["id"] == eid)


def rows_by_key(rows):
    return {r[1]: r for r in rows}


class TheStandard(unittest.TestCase):
    def test_the_repo_file_is_valid(self):
        std = load()
        self.assertEqual(std["schema"], "dstack-nic-standard/1")
        self.assertGreaterEqual(std["version"], 1)

    def test_pending_transmits_stays_8_and_open_with_its_plan(self):
        v = {x["key"]: x for x in entry(load(), "rtl8156b-windows")["values"]}["PendingTransmits"]
        self.assertEqual(v["expected"], "8")
        self.assertEqual(v["status"], "open")
        self.assertIn("#425", v["plan"])

    def test_every_chip_found_in_use_has_an_entry(self):
        std = load()
        self.assertIsNotNone(nsc.find_entry(std, "linux", "usb", "0bda:8156"))
        self.assertIsNotNone(nsc.find_entry(std, "linux", "pci", "10ec:8168"))
        ids = {e["id"] for e in std["adapters"]}
        self.assertTrue({"rtl8156b-windows", "rtl8125-rtl8168-windows", "realtek-vlan-windows"} <= ids)

    def test_an_unmeasured_chip_has_no_entry(self):
        # The RTL8153 is in no production use: no entry, so a checker fails
        # one instead of passing it on a guess.
        std = load()
        self.assertIsNone(nsc.find_entry(std, "linux", "usb", "0bda:8153"))
        self.assertIsNone(nsc.find_entry(std, "linux", "pci", "10ec:b852"))
        self.assertIsNone(nsc.find_entry(std, "windows", "pci", "10ec:8168"))

    def test_the_rtl8125_on_linux_has_its_entry_from_the_fleet(self):
        # The PP editing PC's unused RTL8125 (camera-box's --all run, 20.11).
        std = load()
        self.assertGreaterEqual(std["version"], 4)
        e = nsc.find_entry(std, "linux", "pci", "10ec:8125")
        self.assertIsNotNone(e)
        self.assertEqual(e["id"], "rtl8125-linux")
        self.assertEqual(e["driver"]["modules"], ["r8169"])
        values = {v["key"]: v for v in e["values"]}
        # No link was ever read on one: speed and duplex are no standard yet.
        self.assertEqual(values["link:speed"]["status"], "report")
        self.assertEqual(values["link:duplex"]["status"], "report")
        self.assertEqual(values["ring:rx"]["expected"], "256")
        self.assertNotIn("coalesce:rx-usecs", values)

    def test_validation_refuses_a_broken_file(self):
        good = load()

        def broken(change):
            std = copy.deepcopy(good)
            change(std)
            with self.assertRaises(nsc.StandardError):
                nsc.validate(std)

        values = lambda s: s["adapters"][0]["values"]  # noqa: E731
        broken(lambda s: s.update(schema="other/1"))
        broken(lambda s: s.update(version="1"))
        broken(lambda s: s.update(version=True))
        broken(lambda s: s.pop("adapters"))
        broken(lambda s: values(s)[1].pop("evidence"))
        broken(lambda s: values(s)[1].update(status="report"))
        broken(lambda s: values(s)[1].update(status="Report"))
        broken(lambda s: values(s)[1].update(compare="near"))
        broken(lambda s: values(s)[1].update(absent="PASS"))
        broken(lambda s: values(s)[1].update(profile="lowlatency"))
        broken(lambda s: values(s)[1].update(expected=64))
        broken(lambda s: values(s)[1].update(expected=[]))
        broken(lambda s: values(s)[1].update(key=values(s)[0]["key"].lower()))
        broken(lambda s: values(s)[0].pop("plan"))
        broken(lambda s: [v for v in values(s) if v.get("compare") == "bits"][0].update(expected="0x08"))
        broken(lambda s: s["adapters"][1].update(id=s["adapters"][0]["id"]))
        broken(lambda s: s["adapters"][0]["driver"].pop("evidence"))
        broken(lambda s: s["adapters"][0]["driver"].pop("files"))
        broken(lambda s: s["adapters"][0]["driver"]["bad"][0].pop("evidence"))
        broken(lambda s: s["adapters"][0].update(os="macos"))
        broken(lambda s: s["adapters"][0].update(match={"usb": ["0bda:8156"]}))
        broken(lambda s: s["adapters"][0].pop("values"))
        broken(lambda s: s.update(adpaters=[]))
        broken(lambda s: s["adapters"][0].update(chips="RTL8156B"))
        broken(lambda s: s["adapters"][0]["driver"].update(min_versoin="1"))
        broken(lambda s: values(s)[1].update(expect="64"))
        broken(lambda s: s["adapters"][0]["driver"]["bad"][0].update(why="x"))
        broken(lambda s: s["adapters"][0]["driver"].update(bad={"file": "x.sys", "evidence": "e"}))
        broken(lambda s: s["adapters"][0]["driver"].update(bad=None))
        broken(lambda s: s["adapters"][0]["driver"].update(min_version=11))
        broken(lambda s: s["adapters"][0]["driver"].update(plan=7))
        broken(lambda s: s["adapters"][0]["driver"].update(modules=["r8152"]))
        broken(lambda s: s["adapters"][1]["driver"].update(files=["r8152.ko"]))
        broken(lambda s: s["adapters"][1]["driver"].update(firmware=3))
        broken(lambda s: [v for v in values(s) if v.get("compare") == "bits"][0].update(expected="²"))
        broken(lambda s: [v for v in values(s) if v.get("compare") == "bits"][0].update(expected="1" * 19))

    def test_load_reports_a_missing_or_bad_file(self):
        with self.assertRaises(nsc.StandardError):
            nsc.load_standard("/nonexistent/nic-standard.json")
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            f.write("{not json")
        try:
            with self.assertRaises(nsc.StandardError):
                nsc.load_standard(f.name)
        finally:
            os.unlink(f.name)


class Parsing(unittest.TestCase):
    def test_ethtool_outputs(self):
        f = stream_facts()
        self.assertEqual(f["driver"], "r8152")
        self.assertEqual(f["driver:version"], "v2.21.4 (2025/10/28)")
        self.assertEqual(f["firmware"], "")
        self.assertEqual(f["feature:tcp-segmentation-offload"], "on")
        self.assertEqual(f["feature:tx-checksum-ipv4"], "on")
        self.assertEqual(f["feature:hw-tc-offload"], "off")
        self.assertEqual(f["ring:rx"], "4096")
        self.assertNotIn("ring:tx", f)
        self.assertEqual(f["coalesce:rx-usecs"], "15")
        self.assertEqual(f["coalesce:tx-usecs"], "0")
        self.assertEqual((f["pause:autonegotiate"], f["pause:rx"], f["pause:tx"]), ("on", "off", "off"))
        self.assertEqual(f["eee:status"], "disabled")
        self.assertEqual((f["link:speed"], f["link:duplex"], f["link:autoneg"]), ("2500Mb/s", "Full", "on"))
        self.assertEqual(f["link:detected"], "yes")
        self.assertEqual(nsc.parse_link(NO_LINK)["link:detected"], "no")

    def test_the_ring_reads_the_current_settings_not_the_maximums(self):
        text = "Pre-set maximums:\nRX:\t4096\nTX:\t256\nCurrent hardware settings:\nRX:\t100\nTX:\t256\n"
        self.assertEqual(nsc.parse_ring(text), {"ring:rx": "100", "ring:tx": "256"})

    def test_eee_states(self):
        self.assertEqual(nsc.parse_eee(IEM_EEE), {"eee:status": "enabled"})
        self.assertEqual(nsc.parse_eee(COMPANION_EEE), {"eee:status": "not supported"})
        self.assertEqual(nsc.parse_eee(""), {})

    def test_no_lpm_reads_the_flag_for_this_device_only(self):
        on = {"usbcore:no_lpm": "on"}
        off = {"usbcore:no_lpm": "off"}
        self.assertEqual(nsc.parse_no_lpm("0bda:8156:k", "0bda:8156"), on)
        self.assertEqual(nsc.parse_no_lpm("0bda:8156:gk", "0bda:8156"), on)
        self.assertEqual(nsc.parse_no_lpm("1234:5678:k,0BDA:8156:k", "0bda:8156"), on)
        self.assertEqual(nsc.parse_no_lpm("0bda:8153:k", "0bda:8156"), off)
        self.assertEqual(nsc.parse_no_lpm("0bda:8156:g", "0bda:8156"), off)
        self.assertEqual(nsc.parse_no_lpm("", "0bda:8156"), off)


class Judging(unittest.TestCase):
    def test_holds(self):
        self.assertTrue(nsc.holds({"expected": "8"}, " 8 "))
        self.assertFalse(nsc.holds({"expected": "8"}, "32"))
        self.assertTrue(nsc.holds({"expected": "Full"}, "full"))
        self.assertTrue(nsc.holds({"expected": ["15", "15000"]}, "15000"))
        self.assertFalse(nsc.holds({"expected": ["15", "15000"]}, "150"))
        self.assertTrue(nsc.holds({"expected": "8", "compare": "bits"}, "24"))
        self.assertFalse(nsc.holds({"expected": "8", "compare": "bits"}, "16"))
        self.assertFalse(nsc.holds({"expected": "8", "compare": "bits"}, "x"))
        self.assertFalse(nsc.holds({"expected": "8", "compare": "bits"}, "0x18"))
        self.assertFalse(nsc.holds({"expected": "8", "compare": "bits"}, "²"))
        self.assertFalse(nsc.holds({"expected": "8", "compare": "bits"}, "8" * 19))
        self.assertTrue(nsc.holds({"expected": "8", "compare": "bits"}, "24\n"))
        self.assertTrue(nsc.holds({"expected": "b", "compare": "contains"}, "ABC"))
        self.assertFalse(nsc.holds({"expected": "d", "compare": "contains"}, "abc"))

    def test_absent_value(self):
        must = {"key": "k", "expected": "0", "evidence": "e"}
        may = dict(must, absent="pass")
        self.assertEqual(nsc.judge_value(must, {}, "normal")[0], "FAIL")
        self.assertEqual(nsc.judge_value(may, {}, "normal")[0], "PASS")
        self.assertEqual(nsc.judge_value(may, {"k": "1"}, "normal")[0], "FAIL")

    def test_a_keyword_matches_whatever_its_case(self):
        v = {"key": "*PriorityVLANTag", "expected": "3", "evidence": "e"}
        self.assertEqual(nsc.judge_value(v, {"*PriorityVlanTag": "3"}, "normal")[:4], ("PASS", "*PriorityVLANTag", "3", "3"))

    def test_report_and_profile_and_open(self):
        report = {"key": "k", "expected": None, "status": "report", "evidence": "e"}
        self.assertEqual(nsc.judge_value(report, {"k": "x"}, "normal")[:4], ("INFO", "k", "(no standard yet)", "x"))
        low = {"key": "k", "expected": "off", "profile": "low_latency", "evidence": "e"}
        self.assertEqual(nsc.judge_value(low, {"k": "on"}, "normal")[0], "SKIP")
        self.assertEqual(nsc.judge_value(low, {"k": "on"}, "low_latency")[0], "FAIL")
        opened = {"key": "k", "expected": "8", "status": "open", "plan": "measure it", "evidence": "e"}
        row = nsc.judge_value(opened, {"k": "32"}, "normal")
        self.assertEqual(row[0], "FAIL")
        self.assertEqual(row[4], "open: measure it")

    def test_version_order(self):
        self.assertEqual(nsc.compare_versions("1156.22.113.2026", "11.19.602.2025"), 1)
        self.assertEqual(nsc.compare_versions("11.4.211.2022", "11.19.602.2025"), -1)
        self.assertEqual(nsc.compare_versions("10.79.20.1003", "10.79.50.1003"), -1)
        self.assertEqual(nsc.compare_versions("10.79.20", "10.79.20.0"), 0)

    def test_driver(self):
        std = load()
        win = entry(std, "rtl8156b-windows")["driver"]
        verdict = lambda name, version: [r[0] for r in nsc.judge_driver(win, {"driver": name, "driver:version": version})]  # noqa: E731
        self.assertEqual(verdict("rtucx22x64.sys", "11.19.602.2025"), ["PASS", "PASS"])
        self.assertEqual(verdict("RTUCX22X64.SYS", "11.19.602.2025"), ["PASS", "PASS"])
        self.assertEqual(verdict("rtucx22x64.sys", "11.18.1.2025"), ["PASS", "FAIL"])
        self.assertEqual(verdict("rtucx22x64.sys", ""), ["PASS", "FAIL"])
        self.assertEqual(verdict("rtucx21x64.sys", "11.4.211.2022"), ["FAIL"])
        self.assertEqual(verdict("rtu56cx22x64.sys", "1156.22.113.2026"), ["FAIL"])
        bad = nsc.judge_driver(win, {"driver": "rtump64x64.sys", "driver:version": "10.68.20.605"})
        self.assertIn("#290", bad[0][4])
        pcie = entry(std, "rtl8125-rtl8168-windows")["driver"]
        self.assertEqual(nsc.judge_driver(pcie, {"driver": "rt640x64.sys", "driver:version": "10.72.524.2024"})[0][0], "FAIL")
        good = nsc.judge_driver(pcie, {"driver": "rt640x64.sys", "driver:version": "10.79.50.1003"})
        self.assertEqual([r[0] for r in good], ["PASS", "PASS"])
        self.assertTrue(good[0][4].startswith("open: "))
        r8169 = entry(std, "rtl8168-linux")["driver"]
        rows = nsc.judge_driver(r8169, {"driver": "r8169", "driver:version": "7.0.0-38-realtime", "firmware": "rtl8168h-2_0.0.2 02/26/15"})
        self.assertEqual([r[0] for r in rows], ["PASS", "INFO", "PASS"])
        rows = nsc.judge_driver(r8169, {"driver": "r8169", "firmware": ""})
        self.assertEqual(rows[-1][0], "FAIL")

    def test_stream_snv_against_the_standard(self):
        rows = rows_by_key(nsc.check(entry(load(), "rtl8156b-linux"), stream_facts(), "normal"))
        self.assertEqual(rows["driver"][0], "PASS")
        self.assertEqual(rows["driver:version"][0], "INFO")
        self.assertTrue(rows["driver"][4].startswith("open: "))
        self.assertEqual(rows["feature:tcp-segmentation-offload"][0], "SKIP")
        self.assertEqual(rows["usbcore:no_lpm"][0], "PASS")
        self.assertEqual(rows["firmware"][0], "INFO")
        fails = [k for k, r in rows.items() if r[0] == "FAIL"]
        self.assertEqual(fails, [])
        low = rows_by_key(nsc.check(entry(load(), "rtl8156b-linux"), stream_facts(), "low_latency"))
        self.assertEqual(sorted(k for k, r in low.items() if r[0] == "FAIL"),
                         ["feature:generic-segmentation-offload", "feature:tcp-segmentation-offload"])

    def test_a_nic_without_a_link_gets_no_speed_or_duplex_verdict(self):
        # A cable-less onboard RTL8168 (camera-box's --all runs, 20.11): speed
        # and duplex are INFO "no link"; autoneg, EEE and the rest are judged.
        facts = {"driver": "r8169", "eee:status": "enabled"}
        facts.update(nsc.parse_link(NO_LINK))
        facts["link:autoneg"] = "off"
        rows = rows_by_key(nsc.check(entry(load(), "rtl8168-linux"), facts, "normal"))
        for key in ("link:speed", "link:duplex"):
            self.assertEqual(rows[key][0], "INFO")
            self.assertEqual(rows[key][3], "no link")
        self.assertEqual(rows["link:autoneg"][0], "FAIL")
        self.assertEqual(rows["eee:status"][0], "FAIL")
        self.assertEqual(rows["ring:rx"][0], "FAIL")
        # With a link, a wrong speed still fails.
        facts.update(nsc.parse_link(STREAM_LINK))
        rows = rows_by_key(nsc.check(entry(load(), "rtl8168-linux"), facts, "normal"))
        self.assertEqual(rows["link:speed"][:4], ("FAIL", "link:speed", "1000Mb/s", "2500Mb/s"))
        # No "Link detected" line read: judged as before (absent fails).
        rows = rows_by_key(nsc.check(entry(load(), "rtl8168-linux"), {"driver": "r8169"}, "normal"))
        self.assertEqual(rows["link:speed"][0], "FAIL")

    def test_the_pp_editing_pcs_rtl8125_against_its_entry(self):
        rows = rows_by_key(nsc.check(entry(load(), "rtl8125-linux"), rtl8125_facts(), "normal"))
        self.assertEqual([k for k, r in rows.items() if r[0] == "FAIL"], [])
        self.assertEqual(rows["driver"][0], "PASS")
        self.assertEqual(rows["firmware"][0], "PASS")
        self.assertEqual(rows["link:speed"][:4], ("INFO", "link:speed", "(no standard yet)", "no link"))
        self.assertEqual(rows["feature:tcp-segmentation-offload"][0], "SKIP")
        low = rows_by_key(nsc.check(entry(load(), "rtl8125-linux"), rtl8125_facts(), "low_latency"))
        self.assertEqual(sorted(k for k, r in low.items() if r[0] == "FAIL"),
                         ["feature:generic-segmentation-offload", "feature:tcp-segmentation-offload"])

    def test_a_changed_value_fails_with_both_values(self):
        facts = stream_facts()
        facts["ring:rx"] = "100"
        row = rows_by_key(nsc.check(entry(load(), "rtl8156b-linux"), facts, "normal"))["ring:rx"]
        self.assertEqual(row[:4], ("FAIL", "ring:rx", "4096", "100"))
        line = nsc.format_row("rig0", row)
        self.assertTrue(line.startswith("FAIL  rig0  ring:rx  expected=4096  actual=100  ("))
        self.assertIn("#408", line)
        passed = nsc.format_row("rig0", ("PASS", "ring:rx", "4096", "4096", "evidence"))
        self.assertEqual(passed, "PASS  rig0  ring:rx  expected=4096  actual=4096")

    def test_windows_facts_judged_by_the_same_rules(self):
        # dsg's RTL8168 as read on 20.11.2026 (the keys in its driver's case).
        e = entry(load(), "rtl8125-rtl8168-windows")
        facts = {"*PriorityVlanTag": "0", "CLKREQ": "1", "PnPCapabilities": "24"}
        for v in e["values"]:
            if v.get("status") != "report" and v["key"] not in facts:
                facts[v["key"].lower()] = v["expected"] if isinstance(v["expected"], str) else v["expected"][0]
        rows = rows_by_key(nsc.check(e, dict(facts, driver="rt640x64.sys", **{"driver:version": "10.79.50.1003"}), "low_latency"))
        self.assertEqual([k for k, r in rows.items() if r[0] == "FAIL"], [])
        self.assertEqual(rows["*PriorityVlanTag"][0], "INFO")


# Windows advanced properties as read on 20.11.2026 (keyword, value, display
# name, display value): a hidden keyword has no display name.
MBC_EEE = [
    ("*EEE", "1", "Energy-Efficient Ethernet", "Enabled"),
    ("EEEMaxSupportSpeed", "1000", "EEE Max Support Speed", "1.0 Gbps Full Duplex"),
    ("EnableGreenEthernet", "1", "Green Ethernet", "Enabled"),
    ("AdvancedEEE", "0", "Advanced EEE", "Disabled"),
    ("GigaLite", "1", "Gigabit Lite", "Enabled"),
]
RESOLUME_EEE = [
    ("*EEE", "0", "", ""), ("EEE", "0", "", ""), ("EnableGreenEthernet", "0", "", ""),
    ("AdvancedEEE", "0", "", ""), ("PendingTransmits", "8", "", ""),
]
DSG_EEE = [
    ("*EEE", "0", "Energy-Efficient Ethernet", "Disabled"),
    ("EEEMaxSupportSpeed", "1000", "", ""),
]
WIFI = [("IEEE11nMode", "3", "802.11n/ac/ax Wireless Mode", "4. 802.11ax"), ("GreenFieldCapable", "0", "", "")]


def eee_rule():
    return load()["eee"]


def verdicts(rows):
    return {r[1]: r[0] for r in rows}


class Eee(unittest.TestCase):
    """The vendor-independent EEE rule (owner 20.11.2026, dstack #465)."""

    def test_the_rule_is_in_the_file(self):
        rule = eee_rule()
        self.assertIn("*EEE", rule["windows"]["keywords"])
        self.assertIn("EEELinkAdvertisement", rule["windows"]["keywords"])
        self.assertEqual(rule["windows"]["off"], ["0"])
        self.assertEqual(rule["linux"]["off"], ["disabled", "not supported"])
        self.assertGreaterEqual(load()["version"], 3)

    def test_validation_refuses_a_broken_rule(self):
        good = load()

        def broken(change):
            std = copy.deepcopy(good)
            change(std)
            with self.assertRaises(nsc.StandardError):
                nsc.validate(std)

        broken(lambda s: s.pop("eee"))
        broken(lambda s: s.update(eee=[]))
        broken(lambda s: s["eee"].pop("evidence"))
        broken(lambda s: s["eee"].update(vendor="any"))
        broken(lambda s: s["eee"].pop("windows"))
        broken(lambda s: s["eee"].pop("linux"))
        broken(lambda s: s["eee"]["windows"].update(keywords=[]))
        broken(lambda s: s["eee"]["windows"].update(off="0"))
        broken(lambda s: s["eee"]["windows"].update(display_name="energy[("))
        broken(lambda s: s["eee"]["windows"].update(display_on=""))
        broken(lambda s: s["eee"]["windows"].update(keyword=["*EEE"]))
        broken(lambda s: s["eee"]["linux"].update(off=[]))
        broken(lambda s: s["eee"]["linux"].update(on=["enabled"]))

    def test_linux_states(self):
        rule = eee_rule()
        for text, verdict, actual in (
            (IEM_EEE, "FAIL", "enabled - inactive"),
            ("EEE settings for enp1s0:\n\tEEE status: enabled - active\n", "FAIL", "enabled - active"),
            (STREAM_EEE, "PASS", "disabled"),
            (COMPANION_EEE, "PASS", "not supported"),
            ("netlink error: Operation not supported\n", "PASS", "not supported"),
            ("Cannot get EEE settings: Operation not supported\n", "PASS", "not supported"),
            # Another read's EOPNOTSUPP is no EEE answer: unreadable.
            ("Cannot get wake-on-lan settings: Operation not supported\n", "FAIL", "(unreadable)"),
            ("netlink error: No such device\n", "FAIL", "(unreadable)"),
            ("", "FAIL", "(unreadable)"),
        ):
            row = nsc.judge_eee_linux(rule, text, "r8169")
            self.assertEqual((row[0], row[1], row[3]), (verdict, "eee:status", actual), text)
        self.assertEqual(nsc.judge_eee_linux(rule, STREAM_EEE, "r8152")[2], "disabled|not supported")

    def test_a_linux_fail_names_the_driver(self):
        row = nsc.judge_eee_linux(eee_rule(), IEM_EEE, "r8169")
        line = nsc.format_row("enp1s0", row)
        self.assertTrue(
            line.startswith("FAIL  enp1s0  eee:status  expected=disabled|not supported  actual=enabled - inactive  (driver r8169; "),
            line,
        )

    def test_realtek_visible_keywords_mbc(self):
        rows = nsc.judge_eee_windows(eee_rule(), MBC_EEE, "rtu56cx22x64.sys 1156.22.113.2026")
        self.assertEqual(verdicts(rows), {
            "*EEE": "FAIL", "EEEMaxSupportSpeed": "INFO", "EnableGreenEthernet": "FAIL", "AdvancedEEE": "PASS",
        })
        fail = [r for r in rows if r[1] == "*EEE"][0]
        self.assertEqual((fail[2], fail[3]), ("0", "1 (Enabled)"))
        self.assertTrue(fail[4].startswith("driver rtu56cx22x64.sys 1156.22.113.2026; "))

    def test_realtek_hidden_keywords_resolume(self):
        rows = nsc.judge_eee_windows(eee_rule(), RESOLUME_EEE, "rtucx22x64.sys 11.19.602.2025")
        self.assertEqual(verdicts(rows), {"*EEE": "PASS", "EEE": "PASS", "EnableGreenEthernet": "PASS", "AdvancedEEE": "PASS"})

    def test_a_hidden_keyword_that_is_no_switch_is_not_judged(self):
        rows = nsc.judge_eee_windows(eee_rule(), DSG_EEE, "rt640x64.sys 10.79.50.1003")
        self.assertEqual(verdicts(rows), {"*EEE": "PASS"})

    def test_intel(self):
        rule = eee_rule()
        on = [("EEELinkAdvertisement", "1", "Energy Efficient Ethernet", "On")]
        off = [("eeelinkadvertisement", "0", "Energy Efficient Ethernet", "Off")]
        legacy = [("*EEE", "1", "Energy Efficient Ethernet", "On")]
        self.assertEqual(verdicts(nsc.judge_eee_windows(rule, on, "e2fn.sys 2.1.5.7")), {"EEELinkAdvertisement": "FAIL"})
        self.assertEqual(verdicts(nsc.judge_eee_windows(rule, off, "e2fn.sys 2.1.5.7")), {"eeelinkadvertisement": "PASS"})
        self.assertEqual(verdicts(nsc.judge_eee_windows(rule, legacy, "e1d68x64.sys 12.19.2.45")), {"*EEE": "FAIL"})

    def test_another_vendor_by_its_display_name(self):
        # An Aquantia-style driver: a keyword not in the list, found by its
        # display name and judged by its display value.
        rule = eee_rule()
        on = [("EeeEnable", "1", "Energy-Efficient Ethernet", "Enabled")]
        off = [("EeeEnable", "1", "Energy-Efficient Ethernet", "Disabled")]
        zero = [("EeeEnable", "0", "Energy Efficient Ethernet", "")]
        green = [("GreenMode", "2", "Green Ethernet", "On (at 1 Gbps)")]
        odd = [("EeeEnable", "2", "EEE", "Auto")]
        self.assertEqual(nsc.judge_eee_windows(rule, on, "aqnic650.sys 3.1.6")[0][0], "FAIL")
        self.assertEqual(nsc.judge_eee_windows(rule, off, "aqnic650.sys 3.1.6")[0][0], "PASS")
        self.assertEqual(nsc.judge_eee_windows(rule, zero, "aqnic650.sys 3.1.6")[0][0], "PASS")
        self.assertEqual(nsc.judge_eee_windows(rule, green, "x.sys 1")[0][0], "FAIL")
        self.assertEqual(nsc.judge_eee_windows(rule, odd, "x.sys 1")[0][0], "INFO")

    def test_a_listed_keyword_without_a_value_goes_by_its_display_value(self):
        rule = eee_rule()
        on = [("*EEE", "", "Energy-Efficient Ethernet", "Enabled")]
        off = [("*EEE", None, "Energy-Efficient Ethernet", "Disabled")]
        bare = [("*EEE", "", "", "")]
        self.assertEqual(verdicts(nsc.judge_eee_windows(rule, on, "x.sys 1")), {"*EEE": "FAIL"})
        self.assertEqual(verdicts(nsc.judge_eee_windows(rule, off, "x.sys 1")), {"*EEE": "PASS"})
        self.assertEqual(verdicts(nsc.judge_eee_windows(rule, bare, "x.sys 1")), {"eee": "INFO"})

    def test_run_hands_back_stderr_only_when_asked(self):
        args = [sys.executable, "-c", "import sys; print('out'); print('err', file=sys.stderr); sys.exit(75)"]
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(nsc.run(args), "out\n")
        self.assertEqual(nsc.run(args, with_stderr=True), "out\nerr\n")

    def test_no_eee_keyword_is_info(self):
        rows = nsc.judge_eee_windows(eee_rule(), WIFI, "x.sys 1")
        self.assertEqual(len(rows), 1)
        self.assertEqual((rows[0][0], rows[0][1], rows[0][3]), ("INFO", "eee", "(no EEE keyword)"))


class ThisPc(unittest.TestCase):
    """The I/O layer on a temporary sysfs tree, with ethtool's outputs
    replaced by stream-snv's (nsc.run patched)."""

    def setUp(self):
        self.root = tempfile.TemporaryDirectory()
        root = self.root.name
        self.net = os.path.join(root, "net")
        usbdev = os.path.join(root, "devices", "usb4", "4-4")
        os.makedirs(os.path.join(usbdev, "4-4:1.0", "power"))
        os.makedirs(os.path.join(usbdev, "power"))
        self._write(os.path.join(usbdev, "idVendor"), "0bda")
        self._write(os.path.join(usbdev, "idProduct"), "8156")
        self._write(os.path.join(usbdev, "power", "control"), "on")
        self._write(os.path.join(usbdev, "4-4:1.0", "power", "control"), "auto")
        pcidev = os.path.join(root, "devices", "pci", "0000:00:1c.0", "0000:02:00.0")
        os.makedirs(os.path.join(pcidev, "power"))
        os.makedirs(os.path.join(root, "devices", "pci", "0000:00:1c.0", "power"))
        self._write(os.path.join(pcidev, "vendor"), "0x10ec")
        self._write(os.path.join(pcidev, "device"), "0x8168")
        self._write(os.path.join(pcidev, "power", "control"), "on")
        self._write(os.path.join(root, "devices", "pci", "0000:00:1c.0", "power", "control"), "auto")
        intel = os.path.join(root, "devices", "pci", "0000:07:00.0")
        os.makedirs(intel)
        self._write(os.path.join(intel, "vendor"), "0x8086")
        self._write(os.path.join(intel, "device"), "0x10fb")
        for target, driver in ((os.path.join(usbdev, "4-4:1.0"), "r8152"), (pcidev, "r8169"), (intel, "ixgbe")):
            os.makedirs(os.path.join(root, "drivers", driver), exist_ok=True)
            os.symlink(os.path.join(root, "drivers", driver), os.path.join(target, "driver"))
        for iface, target, state in (
            ("rig0", os.path.join(usbdev, "4-4:1.0"), "up"),
            ("enp2s0", pcidev, "down"),
            ("enp7s0", intel, "up"),
        ):
            os.makedirs(os.path.join(self.net, iface))
            os.symlink(target, os.path.join(self.net, iface, "device"))
            self._write(os.path.join(self.net, iface, "operstate"), state)
        os.makedirs(os.path.join(self.net, "rig0.70"))
        self.quirks = os.path.join(root, "quirks")
        self._write(self.quirks, "0bda:8156:k")
        self._run = nsc.run
        # ethtool --show-eee per interface (stream-snv's "disabled" unless a
        # test sets another), every other ethtool output stream-snv's.
        self.eee = {}
        # ethtool's stderr per interface, handed back only when the caller
        # asks for it (with_stderr), as nsc.run does.
        self.eee_stderr = {}
        # `ethtool <iface>` per interface (stream-snv's link unless a test
        # sets another).
        self.link = {}
        nsc.run = lambda args, with_stderr=False: (
            self.eee.get(args[-1], STREAM_EEE) + (self.eee_stderr.get(args[-1], "") if with_stderr else "")
            if args[1] == "--show-eee"
            else STREAM_OUTPUTS[args[1]] if len(args) > 2
            else self.link.get(args[-1], STREAM_LINK)
        )

    def tearDown(self):
        nsc.run = self._run
        self.root.cleanup()

    @staticmethod
    def _write(path, text):
        with open(path, "w") as f:
            f.write(text + "\n")

    def main(self, *argv, standard=STANDARD):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = nsc.main(["--standard", standard, *argv], sysfs=self.net, quirks_path=self.quirks)
        return rc, out.getvalue().splitlines()

    def test_nic_ids_from_sysfs(self):
        self.assertEqual(nsc.nic_ids("rig0", self.net), ("usb", "0bda:8156"))
        self.assertEqual(nsc.nic_ids("enp2s0", self.net), ("pci", "10ec:8168"))
        self.assertIsNone(nsc.nic_ids("rig0.70", self.net))

    def test_power_nodes_are_the_nics_own(self):
        usb = nsc.read_facts("rig0", ("usb", "0bda:8156"), self.net, self.quirks)
        pci = nsc.read_facts("enp2s0", ("pci", "10ec:8168"), self.net, self.quirks)
        self.assertEqual(usb["usb:power/control"], "on")
        self.assertEqual(usb["usbcore:no_lpm"], "on")
        self.assertEqual(pci["pci:power/control"], "on")
        self.assertNotIn("usbcore:no_lpm", pci)

    def test_a_clean_pc_exits_0_and_skips_a_down_nic(self):
        rc, lines = self.main()
        self.assertEqual(rc, 0)
        self.assertIn("SKIP  enp2s0  pci 10ec:8168 is down", lines)
        self.assertIn("PASS  rig0  ring:rx  expected=4096  actual=4096", lines)
        # enp7s0 (Intel) gets the EEE rule only, never a Realtek value.
        self.assertEqual(
            [line for line in lines if "enp7s0" in line],
            ["PASS  enp7s0  eee:status  expected=disabled|not supported  actual=disabled"],
        )
        self.assertEqual(lines[-1], "1 Realtek NIC(s) checked, 0 FAIL")

    def test_a_down_nic_is_checked_with_all(self):
        rc, lines = self.main("--all")
        self.assertEqual(rc, 1)
        self.assertIn("--    enp2s0  RTL8168 (rtl8168-linux)", lines)

    def test_a_failed_value_exits_1(self):
        self._write(self.quirks, "")
        rc, lines = self.main("rig0")
        self.assertEqual(rc, 1)
        self.assertTrue(any(line.startswith("FAIL  rig0  usbcore:no_lpm  expected=on  actual=off") for line in lines))

    def test_a_chip_without_an_entry_fails(self):
        self._write(os.path.join(self.root.name, "devices", "usb4", "4-4", "idProduct"), "8153")
        rc, lines = self.main()
        self.assertEqual(rc, 1)
        self.assertIn("FAIL  rig0  usb 0bda:8153: no standard for this Realtek chip on Linux", lines)

    def test_a_named_interface_that_is_missing_or_no_realtek_fails(self):
        rc, lines = self.main("rgi0", "enp7s0")
        self.assertEqual(rc, 1)
        self.assertIn("FAIL  rgi0: no such Realtek NIC", lines)
        self.assertIn("FAIL  enp7s0: no such Realtek NIC", lines)

    def test_eee_is_checked_on_every_wired_nic_whatever_the_vendor_or_state(self):
        rc, lines = self.main()
        self.assertEqual(rc, 0)
        for iface in ("enp2s0", "enp7s0", "rig0"):
            self.assertIn(f"PASS  {iface}  eee:status  expected=disabled|not supported  actual=disabled", lines)
        self.assertIn("3 wired NIC(s) checked for EEE, 0 FAIL", lines)
        self.assertFalse(any("rig0.70" in line for line in lines))

    def test_eee_enabled_on_a_down_or_non_realtek_nic_fails(self):
        self.eee["enp2s0"] = IEM_EEE
        self.eee["enp7s0"] = "EEE settings for enp7s0:\n\tEEE status: enabled - active\n"
        rc, lines = self.main()
        self.assertEqual(rc, 1)
        expected = "eee:status  expected=disabled|not supported  actual=enabled"
        self.assertTrue(any(line.startswith(f"FAIL  enp2s0  {expected} - inactive  (driver r8169; ") for line in lines))
        self.assertTrue(any(line.startswith(f"FAIL  enp7s0  {expected} - active  (driver ixgbe; ") for line in lines))
        self.assertIn("3 wired NIC(s) checked for EEE, 2 FAIL", lines)
        self.assertEqual(lines[-1], "1 Realtek NIC(s) checked, 0 FAIL")

    def test_eee_ignores_the_named_interfaces_and_wireless(self):
        os.makedirs(os.path.join(self.net, "wlan0", "wireless"))
        intel = os.path.join(self.root.name, "devices", "pci", "0000:07:00.0")
        os.symlink(intel, os.path.join(self.net, "wlan0", "device"))
        self.eee["enp7s0"] = IEM_EEE
        rc, lines = self.main("rig0")
        self.assertEqual(rc, 1)
        self.assertTrue(any(line.startswith("FAIL  enp7s0  eee:status") for line in lines))
        self.assertFalse(any("wlan0" in line for line in lines))
        self.assertEqual(lines[-1], "1 Realtek NIC(s) checked, 0 FAIL")

    def test_eee_reads_ethtools_unsupported_answer_from_stderr(self):
        # ixgbe on the PP editing PC: nothing on stdout, the answer on stderr.
        self.eee["enp7s0"] = ""
        self.eee_stderr["enp7s0"] = "netlink error: Operation not supported\n"
        rc, lines = self.main()
        self.assertEqual(rc, 0)
        self.assertIn("PASS  enp7s0  eee:status  expected=disabled|not supported  actual=not supported", lines)

    def test_eee_skips_a_non_ethernet_or_wireless_interface(self):
        intel = os.path.join(self.root.name, "devices", "pci", "0000:07:00.0")
        for iface, marker in (("ib0", None), ("wlp1s0", "phy80211")):
            os.makedirs(os.path.join(self.net, iface))
            os.symlink(intel, os.path.join(self.net, iface, "device"))
            if marker:
                os.makedirs(os.path.join(self.net, iface, marker))
        self._write(os.path.join(self.net, "ib0", "type"), "32")
        self._write(os.path.join(self.net, "enp7s0", "type"), "1")
        self.assertEqual(nsc.wired_nics(self.net), ["enp2s0", "enp7s0", "rig0"])

    def _realtek_wifi(self, iface, markers=("wireless", "phy80211")):
        """A Realtek WiFi (RTL8852BE, 10ec:b852, class 0x028000, as on the
        SNV editing PC and both stream PCs, 20.11.2026), down."""
        wifi = os.path.join(self.root.name, "devices", "pci", "0000:04:00.0")
        os.makedirs(wifi, exist_ok=True)
        self._write(os.path.join(wifi, "vendor"), "0x10ec")
        self._write(os.path.join(wifi, "device"), "0xb852")
        self._write(os.path.join(wifi, "class"), "0x028000")
        os.makedirs(os.path.join(self.net, iface))
        os.symlink(wifi, os.path.join(self.net, iface, "device"))
        self._write(os.path.join(self.net, iface, "operstate"), "down")
        for marker in markers:
            os.makedirs(os.path.join(self.net, iface, marker))

    def test_a_down_nic_without_a_link_gets_no_speed_or_duplex_fail(self):
        # camera-box's --all runs (20.11): a cable-less onboard RTL8168.
        self.link["enp2s0"] = NO_LINK
        rc, lines = self.main("--all")
        self.assertIn("--    enp2s0  RTL8168 (rtl8168-linux)", lines)
        for key in ("link:speed", "link:duplex"):
            line = next(x for x in lines if x.startswith(f"INFO  enp2s0  {key}  "))
            self.assertIn("  actual=no link  (", line)
        self.assertFalse(any(x.startswith("FAIL  enp2s0  link:") for x in lines))
        # Its other values are still judged (stream-snv's ring is no RTL8168's).
        self.assertTrue(any(x.startswith("FAIL  enp2s0  ring:rx  expected=256  actual=4096") for x in lines))
        self.assertIn("PASS  enp2s0  eee:status  expected=disabled|not supported  actual=disabled", lines)
        self.assertEqual(rc, 1)

    def test_a_realtek_wifi_is_out_of_the_wired_scan(self):
        self._realtek_wifi("wlp4s0")
        rc, lines = self.main("--all", "--profile", "normal")
        self.assertFalse(any("wlp4s0" in x for x in lines))
        self.assertIn("3 wired NIC(s) checked for EEE, 0 FAIL", lines)
        self.assertEqual(lines[-1].split(" Realtek")[0], "2")

    def test_a_wifi_by_its_pci_class_alone_is_out_of_the_wired_scan(self):
        # No wireless/phy80211 node (no driver bound): the PCI class 0x0280
        # (network controller, not Ethernet 0x0200) still tells it.
        self._realtek_wifi("wlp4s0", markers=())
        self.assertTrue(nsc.is_wireless("wlp4s0", self.net))
        self.assertFalse(nsc.is_wireless("enp2s0", self.net))
        self.assertNotIn("wlp4s0", nsc.wired_nics(self.net))
        rc, lines = self.main("--all")
        self.assertFalse(any("wlp4s0" in x for x in lines))

    def test_a_named_wifi_fails_as_no_wired_realtek(self):
        self._realtek_wifi("wlp4s0")
        rc, lines = self.main("wlp4s0")
        self.assertEqual(rc, 1)
        self.assertIn("FAIL  wlp4s0: no such Realtek NIC", lines)
        self.assertEqual(lines[-1], "0 Realtek NIC(s) checked, 1 FAIL")

    def test_an_unreadable_standard_exits_2(self):
        rc, lines = self.main(standard=os.path.join(self.root.name, "missing.json"))
        self.assertEqual(rc, 2)
        self.assertTrue(lines[0].startswith("FAIL  cannot read the standard"))


if __name__ == "__main__":
    unittest.main()
