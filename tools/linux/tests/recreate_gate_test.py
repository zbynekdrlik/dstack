# SPDX-License-Identifier: MIT OR Apache-2.0
"""recreate_gate.py's reads (dstack #411, #414, #451), run without root:
python3 -m unittest tools/linux/tests/recreate_gate_test.py"""

import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import threading
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


def lanes(*direct):
    return {"device": {"tx": [{"flow": i + 1, "packets": 9, "direct": d} for i, d in enumerate(direct)]}}


class Direct(unittest.TestCase):
    def test_sums_every_tx_lane_s_direct_packets(self):
        self.assertEqual(gate.direct(lanes(100, 0, 23)), 123)

    def test_no_lane_is_none_sent(self):
        self.assertEqual(gate.direct(lanes()), 0)
        self.assertEqual(gate.direct({}), 0)
        self.assertEqual(gate.direct({"device": {"tx": [{"flow": 1}]}}), 0)

    def test_a_file_not_readable_yet_reads_minus_one(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "a.json")
            self.assertEqual(gate.read_direct(path), -1, "no file")
            with open(path, "w", encoding="utf-8") as f:
                f.write('{"device": {"tx": [')
            self.assertEqual(gate.read_direct(path), -1, "half written")
            with open(path, "w", encoding="utf-8") as f:
                json.dump(lanes(7, 8), f)
            self.assertEqual(gate.read_direct(path), 15)


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


ARC = "recreate-a._netaudio-arc._udp.local."


def response(qid, *records, flags=0x8400, question=None):
    """A DNS response with id `qid`: an optional question, then each
    (name, rtype) as an answer whose name points back to the question's
    when it is the same name (compression, as mdns-sd writes it)."""
    msg = struct.pack(">HHHHHH", qid, flags, 1 if question else 0, len(records), 0, 0)
    at = None
    if question:
        at = len(msg)
        msg += gate.name_bytes(question) + struct.pack(">HH", gate.SRV, gate.IN)
    for name, rtype in records:
        if at is not None and name == question:
            msg += struct.pack(">H", 0xC000 | at)
        else:
            msg += gate.name_bytes(name)
        rdata = b"\0\0\0\0\x11\x58" + gate.name_bytes("host.local.")
        msg += struct.pack(">HHIH", rtype, 0x8001, 120, len(rdata)) + rdata
    return msg


class Query(unittest.TestCase):
    def test_one_srv_question_for_the_instance(self):
        q = gate.query(ARC, 0x1234)
        self.assertEqual(struct.unpack_from(">HHHHHH", q), (0x1234, 0, 1, 0, 0, 0))
        name, off = gate.read_name(q, 12)
        self.assertEqual(name, ARC)
        self.assertEqual(struct.unpack_from(">HH", q, off), (gate.SRV, gate.IN))
        self.assertEqual(len(q), off + 4)

    def test_a_name_without_the_root_dot_is_the_same(self):
        self.assertEqual(gate.name_bytes(ARC), gate.name_bytes(ARC.rstrip(".")))

    def test_an_empty_or_long_label_is_an_error(self):
        with self.assertRaises(ValueError):
            gate.name_bytes("a..local.")
        with self.assertRaises(ValueError):
            gate.name_bytes("x" * 64 + ".local.")


class SrvAnswered(unittest.TestCase):
    def test_the_instance_s_srv_in_the_answers(self):
        self.assertTrue(gate.srv_answered(response(7, (ARC, gate.SRV)), 7, ARC))

    def test_a_compressed_name_and_any_case(self):
        msg = response(7, ("_x._udp.local.", 16), (ARC, gate.SRV), question=ARC)
        self.assertTrue(gate.srv_answered(msg, 7, ARC.upper()))

    def test_another_id_name_or_type_is_no_answer(self):
        self.assertFalse(gate.srv_answered(response(8, (ARC, gate.SRV)), 7, ARC), "id")
        self.assertFalse(gate.srv_answered(response(7, ("other." + ARC, gate.SRV)), 7, ARC), "name")
        self.assertFalse(gate.srv_answered(response(7, (ARC, 16)), 7, ARC), "TXT only")
        self.assertFalse(gate.srv_answered(response(7), 7, ARC), "no answer")

    def test_a_query_is_no_answer(self):
        self.assertFalse(gate.srv_answered(response(7, (ARC, gate.SRV), flags=0), 7, ARC))
        self.assertFalse(gate.srv_answered(gate.query(ARC, 7), 7, ARC))

    def test_a_cut_or_looping_message_is_no_answer(self):
        whole = response(7, (ARC, gate.SRV))
        for n in range(len(whole) - 1):
            self.assertFalse(gate.srv_answered(whole[:n], 7, ARC), n)
        loop = struct.pack(">HHHHHH", 7, 0x8400, 0, 1, 0, 0) + struct.pack(">H", 0xC000 | 12)
        self.assertFalse(gate.srv_answered(loop, 7, ARC))


class AnsweredOnce(unittest.TestCase):
    A = "10.71.0.1"

    def test_one_answer_from_the_address_s_5353(self):
        self.assertTrue(gate.answered_once([(self.A, 5353, True)], self.A))

    def test_replies_that_do_not_answer_do_not_count(self):
        self.assertTrue(gate.answered_once([(self.A, 5353, False), (self.A, 5353, True)], self.A))
        self.assertFalse(gate.answered_once([(self.A, 5353, False)], self.A))

    def test_no_answer_is_a_failure(self):
        self.assertFalse(gate.answered_once([], self.A))

    def test_two_answers_are_a_failure(self):
        self.assertFalse(gate.answered_once([(self.A, 5353, True), (self.A, 5353, True)], self.A))

    def test_an_answer_from_elsewhere_is_a_failure(self):
        self.assertFalse(gate.answered_once([("10.71.0.2", 5353, True)], self.A))
        self.assertFalse(gate.answered_once([(self.A, 40000, True)], self.A))
        self.assertFalse(gate.answered_once([(self.A, 5353, True), ("10.71.0.2", 5353, True)], self.A))


class Responder:
    """A loopback stand-in for a device's mDNS daemon on `ADDRESS`:5353:
    answers each query `times` times with the asked id (0 = silent)."""

    ADDRESS = "127.0.0.9"

    def __init__(self, times, instance=ARC):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind((self.ADDRESS, gate.MDNS_PORT))
        self.sock.settimeout(5)
        self.times = times
        self.instance = instance
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def serve(self):
        try:
            msg, src = self.sock.recvfrom(9000)
        except OSError:
            return
        (qid,) = struct.unpack_from(">H", msg)
        for _ in range(self.times):
            self.sock.sendto(response(qid, (self.instance, gate.SRV)), src)

    def close(self):
        self.thread.join(6)
        self.sock.close()


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

    def test_direct_prints_the_sum(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "a.json")
            with open(path, "w", encoding="utf-8") as f:
                json.dump(lanes(3000, 2), f)
            r = self.run_gate("direct", path)
            self.assertEqual((r.returncode, r.stdout), (0, "3002\n"))
            r = self.run_gate("direct", os.path.join(d, "none.json"))
            self.assertEqual((r.returncode, r.stdout), (0, "-1\n"))

    def test_index_reads_stdin(self):
        r = self.run_gate("index", "rig0", stdin=Index.LINKS)
        self.assertEqual((r.returncode, r.stdout), (0, "12\n"))

    def test_rose_is_its_exit_code(self):
        self.assertEqual(self.run_gate("rose", "0", "1500", "1000").returncode, 0)
        self.assertEqual(self.run_gate("rose", "0", "999", "1000").returncode, 1)
        self.assertEqual(self.run_gate("rose", "-1", "1500", "1000").returncode, 1)

    def ask(self, times, instance=ARC):
        responder = Responder(times, instance)
        try:
            return self.run_gate("ask", Responder.ADDRESS, ARC, "0.5")
        finally:
            responder.close()

    def test_ask_passes_on_one_answer_from_the_address(self):
        r = self.ask(1)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(
            r.stdout,
            f"reply from {Responder.ADDRESS}:5353: answers {ARC}\n"
            f"{ARC} from {Responder.ADDRESS}:5353: answered once (1 replies in 0.5 s)\n",
        )

    def test_ask_fails_on_silence_two_answers_or_another_record(self):
        for times, instance, replies in ((0, ARC, 0), (2, ARC, 2), (1, "other." + ARC, 1)):
            r = self.ask(times, instance)
            self.assertEqual(r.returncode, 1, (times, instance, r.stdout, r.stderr))
            self.assertIn(f"NOT answered once ({replies} replies", r.stdout)

    def test_a_wrong_call_is_a_usage_error(self):
        r = self.run_gate("flow", "only-one")
        self.assertEqual(r.returncode, 2)
        self.assertIn("usage", r.stderr)
        r = self.run_gate("ask", Responder.ADDRESS, ARC)
        self.assertEqual(r.returncode, 2)
        self.assertIn("ask <address> <instance> <wait_s>", r.stderr)


if __name__ == "__main__":
    unittest.main()
