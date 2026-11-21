#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""dstack #411, #414, #451: the reads of recreate.test.sh, testable without root.

The test re-creates an interface under two running peers and judges from
their status files whether each one hears the other again. What it reads
lives here, with its unit tests (recreate_gate_test.py):

- `flow <status.json> <device>`: the RX packets this peer counted from
  `device` (every flow from it summed) and whether one of those flows is
  multicast, printed as `<packets> <0|1>`; `-1 -1` while the file cannot
  be read yet (a status written while we read it, or none yet).
- `direct <status.json>`: the packets this peer sent past the qdisc
  (`media::bypass`, every TX lane's `direct` summed, dstack #414); -1
  while the file cannot be read yet.
- `index <name>`: the interface `name`'s index from `ip -o link show`
  output on stdin.
- `rose <before> <after> <min>`: exit 0 when a packet count rose by at
  least `min` (a flow delivering), 1 otherwise; an unread count (-1) never
  rose.
- `ask <address> <instance> <wait_s>` (dstack #451): one direct mDNS query
  (RFC 6762 section 5.5: to `<address>:5353`, not the group) for the SRV
  record `instance` (a device's own `_netaudio-arc._udp` record), sent from
  an ephemeral port, so the daemon answers it unicast to that port (a
  legacy query, RFC 6762 section 6.7). Every datagram that comes back
  within `wait_s` is printed, one line each; exit 0 when the record was
  answered once, from `<address>:5353` (`answered_once`), 1 otherwise.
"""

import json
import random
import socket
import struct
import sys
import time

MDNS_PORT = 5353
SRV = 33
IN = 1


def flow(status, device):
    """(packets, multicast) of `status`'s RX flows from `device`."""
    flows = [f for f in status.get("device", {}).get("rx", []) if f.get("device") == device]
    packets = sum(int(f.get("packets", 0)) for f in flows)
    multicast = int(any(f.get("multicast", 0) == 1 for f in flows))
    return packets, multicast


def read_flow(path, device):
    """`flow` of the status file at `path`; (-1, -1) while unreadable."""
    status = read_status(path)
    return (-1, -1) if status is None else flow(status, device)


def read_direct(path):
    """`direct` of the status file at `path`; -1 while unreadable."""
    status = read_status(path)
    return -1 if status is None else direct(status)


def direct(status):
    """The packets `status`'s TX lanes sent past the qdisc, summed."""
    return sum(int(t.get("direct", 0)) for t in status.get("device", {}).get("tx", []))


def read_status(path):
    """The status file at `path`; None while unreadable."""
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def ifindex(text, name):
    """The index of interface `name` in `ip -o link show` output."""
    for line in text.splitlines():
        parts = line.split(":", 2)
        if len(parts) == 3 and parts[1].strip().split("@")[0] == name:
            return int(parts[0])
    raise ValueError(f"no interface {name!r} in: {text!r}")


def rose(before, after, minimum):
    """Whether a count rose by at least `minimum`; an unread one never."""
    return before >= 0 and after >= 0 and after - before >= minimum


def query(instance, qid):
    """A DNS query (flags 0) for the SRV record `instance`, id `qid`."""
    return struct.pack(">HHHHHH", qid, 0, 1, 0, 0, 0) + name_bytes(instance) + struct.pack(">HH", SRV, IN)


def name_bytes(name):
    """`name` as DNS labels, ending in the root label."""
    out = b""
    for label in name.rstrip(".").split("."):
        raw = label.encode("utf-8")
        if not 0 < len(raw) < 64:
            raise ValueError(f"bad label {label!r} in {name!r}")
        out += bytes([len(raw)]) + raw
    return out + b"\0"


def read_name(msg, off):
    """(name, offset after it) of the name at `off`, compression followed."""
    labels = []
    end = None
    for _ in range(128):
        n = msg[off]
        if n & 0xC0 == 0xC0:
            if end is None:
                end = off + 2
            off = ((n & 0x3F) << 8) | msg[off + 1]
            continue
        if n == 0:
            return ".".join(labels) + ".", off + 1 if end is None else end
        labels.append(msg[off + 1 : off + 1 + n].decode("utf-8", "replace"))
        off += 1 + n
    raise ValueError("a name's pointers loop")


def srv_answered(msg, qid, instance):
    """Whether `msg` is a response with id `qid` whose answer section holds
    the SRV record `instance` (names compared case-insensitively); False
    for anything that does not parse."""
    try:
        rid, flags, qd, an, _, _ = struct.unpack_from(">HHHHHH", msg, 0)
        if rid != qid or not flags & 0x8000:
            return False
        off = 12
        for _ in range(qd):
            _, off = read_name(msg, off)
            off += 4
        want = instance.rstrip(".").lower() + "."
        for _ in range(an):
            name, off = read_name(msg, off)
            rtype, _, _, rdlen = struct.unpack_from(">HHIH", msg, off)
            off += 10 + rdlen
            if off > len(msg):
                return False
            if rtype == SRV and name.lower() == want:
                return True
        return False
    except (IndexError, struct.error, ValueError):
        return False


def answered_once(replies, address):
    """The verdict of `ask`: of `replies` ((source ip, source port,
    answered) each), exactly one answered the record, and it came from
    `address`:5353. Two answers (two sockets answering, #444) or one from
    another source fail too."""
    answers = [(ip, port) for ip, port, answered in replies if answered]
    return answers == [(address, MDNS_PORT)]


def ask(address, instance, wait_s):
    """Sends one direct query for `instance` to `address`:5353 and collects
    every reply for `wait_s` seconds: [(source ip, source port, answered)]."""
    qid = random.randint(1, 0xFFFF)
    replies = []
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind(("0.0.0.0", 0))
        sock.sendto(query(instance, qid), (address, MDNS_PORT))
        deadline = time.monotonic() + wait_s
        while (left := deadline - time.monotonic()) > 0:
            sock.settimeout(left)
            try:
                msg, (ip, port) = sock.recvfrom(9000)
            except TimeoutError:
                break
            replies.append((ip, port, srv_answered(msg, qid, instance)))
    return replies


def main(argv):
    cmd = argv[1] if len(argv) > 1 else ""
    if cmd == "flow" and len(argv) == 4:
        packets, multicast = read_flow(argv[2], argv[3])
        print(packets, multicast)
        return 0
    if cmd == "direct" and len(argv) == 3:
        print(read_direct(argv[2]))
        return 0
    if cmd == "index" and len(argv) == 3:
        print(ifindex(sys.stdin.read(), argv[2]))
        return 0
    if cmd == "rose" and len(argv) == 5:
        return 0 if rose(int(argv[2]), int(argv[3]), int(argv[4])) else 1
    if cmd == "ask" and len(argv) == 5:
        replies = ask(argv[2], argv[3], float(argv[4]))
        for ip, port, answered in replies:
            print(f"reply from {ip}:{port}: {'answers' if answered else 'does not answer'} {argv[3]}")
        ok = answered_once(replies, argv[2])
        print(f"{argv[3]} from {argv[2]}:{MDNS_PORT}: {'answered once' if ok else 'NOT answered once'} ({len(replies)} replies in {argv[4]} s)")
        return 0 if ok else 1
    print(
        "usage: recreate_gate.py flow <status.json> <device> | direct <status.json> | "
        "index <name> | "
        "rose <before> <after> <min> | ask <address> <instance> <wait_s>",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
