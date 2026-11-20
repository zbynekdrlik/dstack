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
        # RTL8153 and a Linux RTL8125 are in no production use: no entry, so a
        # checker fails one instead of passing it on a guess.
        std = load()
        self.assertIsNone(nsc.find_entry(std, "linux", "usb", "0bda:8153"))
        self.assertIsNone(nsc.find_entry(std, "linux", "pci", "10ec:8125"))
        self.assertIsNone(nsc.find_entry(std, "windows", "pci", "10ec:8168"))

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
        nsc.run = lambda args: STREAM_OUTPUTS[args[1] if len(args) > 2 else ""]

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
        self.assertFalse(any("enp7s0" in line for line in lines))
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

    def test_an_unreadable_standard_exits_2(self):
        rc, lines = self.main(standard=os.path.join(self.root.name, "missing.json"))
        self.assertEqual(rc, 2)
        self.assertTrue(lines[0].startswith("FAIL  cannot read the standard"))


if __name__ == "__main__":
    unittest.main()
