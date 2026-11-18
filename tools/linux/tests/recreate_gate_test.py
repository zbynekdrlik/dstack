# SPDX-License-Identifier: MIT OR Apache-2.0
"""recreate_gate.py's reads (dstack #411), run without root:
python3 -m unittest tools/linux/tests/recreate_gate_test.py"""

import json
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import recreate_gate as gate  # noqa: E402

GATE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "recreate_gate.py")


def status(*flows):
    return {"device": {"rx": list(flows)}}


def rx(device, packets, multicast=0):
    return {"device": device, "packets": packets, "multicast": multicast}


class Flow(unittest.TestCase):
    def test_sums_every_flow_from_the_device_only(self):
        s = status(rx("b", 100), rx("c", 7, 1), rx("b", 20))
        self.assertEqual(gate.flow(s, "b"), (120, 0))
        self.assertEqual(gate.flow(s, "c"), (7, 1))

    def test_multicast_when_any_flow_from_the_device_is(self):
        self.assertEqual(gate.flow(status(rx("b", 5), rx("b", 6, 1)), "b"), (11, 1))

    def test_no_flow_is_none_heard(self):
        self.assertEqual(gate.flow(status(rx("c", 9, 1)), "b"), (0, 0))
        self.assertEqual(gate.flow({}, "b"), (0, 0))
        self.assertEqual(gate.flow({"device": {}}, "b"), (0, 0))

    def test_a_file_not_readable_yet_reads_minus_one(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "a.json")
            self.assertEqual(gate.read_flow(path, "b"), (-1, -1), "no file")
            with open(path, "w", encoding="utf-8") as f:
                f.write('{"device": {"rx": [')
            self.assertEqual(gate.read_flow(path, "b"), (-1, -1), "half written")
            with open(path, "w", encoding="utf-8") as f:
                json.dump(status(rx("b", 42, 1)), f)
            self.assertEqual(gate.read_flow(path, "b"), (42, 1))


class Index(unittest.TestCase):
    LINKS = (
        "1: lo: <LOOPBACK,UP,LOWER_UP> mtu 65536 qdisc noqueue state UNKNOWN\\    link/loopback\n"
        "12: rig0: <BROADCAST,NOARP,UP,LOWER_UP> mtu 1500 qdisc noqueue state UNKNOWN\\    link/ether\n"
        "13: rig0.70@rig0: <BROADCAST,MULTICAST,UP> mtu 1500\\    link/ether\n"
    )

    def test_the_named_interface_s_index(self):
        self.assertEqual(gate.ifindex(self.LINKS, "rig0"), 12)
        self.assertEqual(gate.ifindex(self.LINKS, "lo"), 1)
        self.assertEqual(gate.ifindex(self.LINKS, "rig0.70"), 13)

    def test_a_missing_interface_is_an_error(self):
        with self.assertRaises(ValueError):
            gate.ifindex(self.LINKS, "rig")


class Rose(unittest.TestCase):
    def test_at_least_the_minimum(self):
        self.assertTrue(gate.rose(100, 1100, 1000))
        self.assertFalse(gate.rose(100, 1099, 1000))
        self.assertFalse(gate.rose(100, 100, 1))

    def test_an_unread_count_never_rose(self):
        self.assertFalse(gate.rose(-1, 5000, 1000))
        self.assertFalse(gate.rose(0, -1, 0))
        self.assertTrue(gate.rose(0, 0, 0))

    def test_a_count_that_fell_never_rose(self):
        # A flow replaced by a new one starts again from 0.
        self.assertFalse(gate.rose(5000, 200, 0))


class Cli(unittest.TestCase):
    def run_gate(self, *args, stdin=""):
        return subprocess.run(
            [sys.executable, "-I", GATE, *args],
            input=stdin,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_flow_prints_packets_and_multicast(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "a.json")
            with open(path, "w", encoding="utf-8") as f:
                json.dump(status(rx("b", 3000, 1)), f)
            r = self.run_gate("flow", path, "b")
            self.assertEqual((r.returncode, r.stdout), (0, "3000 1\n"))

    def test_index_reads_stdin(self):
        r = self.run_gate("index", "rig0", stdin=Index.LINKS)
        self.assertEqual((r.returncode, r.stdout), (0, "12\n"))

    def test_rose_is_its_exit_code(self):
        self.assertEqual(self.run_gate("rose", "0", "1500", "1000").returncode, 0)
        self.assertEqual(self.run_gate("rose", "0", "999", "1000").returncode, 1)
        self.assertEqual(self.run_gate("rose", "-1", "1500", "1000").returncode, 1)

    def test_a_wrong_call_is_a_usage_error(self):
        r = self.run_gate("flow", "only-one")
        self.assertEqual(r.returncode, 2)
        self.assertIn("usage", r.stderr)


if __name__ == "__main__":
    unittest.main()
