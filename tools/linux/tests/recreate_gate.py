#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""dstack #411: the reads of recreate.test.sh, testable without root.

The test re-creates a dummy interface under two running peers and judges
from their status files whether each one hears the other again. What it
reads lives here, with its unit tests (recreate_gate_test.py):

- `flow <status.json> <device>`: the RX packets this peer counted from
  `device` (every flow from it summed) and whether one of those flows is
  multicast, printed as `<packets> <0|1>`; `-1 -1` while the file cannot
  be read yet (a status written while we read it, or none yet).
- `index <name>`: the interface `name`'s index from `ip -o link show`
  output on stdin.
- `rose <before> <after> <min>`: exit 0 when a packet count rose by at
  least `min` (a flow delivering), 1 otherwise; an unread count (-1) never
  rose.
"""

import json
import sys


def flow(status, device):
    """(packets, multicast) of `status`'s RX flows from `device`."""
    flows = [f for f in status.get("device", {}).get("rx", []) if f.get("device") == device]
    packets = sum(int(f.get("packets", 0)) for f in flows)
    multicast = int(any(f.get("multicast", 0) == 1 for f in flows))
    return packets, multicast


def read_flow(path, device):
    """`flow` of the status file at `path`; (-1, -1) while unreadable."""
    try:
        with open(path, encoding="utf-8") as f:
            status = json.load(f)
    except (OSError, ValueError):
        return -1, -1
    return flow(status, device)


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


def main(argv):
    cmd = argv[1] if len(argv) > 1 else ""
    if cmd == "flow" and len(argv) == 4:
        packets, multicast = read_flow(argv[2], argv[3])
        print(packets, multicast)
        return 0
    if cmd == "index" and len(argv) == 3:
        print(ifindex(sys.stdin.read(), argv[2]))
        return 0
    if cmd == "rose" and len(argv) == 5:
        return 0 if rose(int(argv[2]), int(argv[3]), int(argv[4])) else 1
    print(
        "usage: recreate_gate.py flow <status.json> <device> | index <name> | "
        "rose <before> <after> <min>",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
