#!/bin/bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# The generic Dante-leg filter (tools/linux/dante-leg.nft, dstack #264)
# loaded in a private network namespace (run as root, CI's linux-tools
# job): it loads, a second load replaces it (one table, the same rules), a
# load without the interface fails and leaves the loaded table as it was.
# The same for dante-leg-notrack.nft, which must also untrack every packet
# not on the leg and leave the leg's packets tracked.
# The port sets themselves are checked by crates/dstack/tests/dante_leg.rs.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
rules="$here/../dante-leg.nft"
notrack="$here/../dante-leg-notrack.nft"

if [ "${1:-}" != "--inside" ]; then
    exec unshare --net bash "$0" --inside
fi

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

nft -D dante_leg=lo -f "$rules"
first="$(nft list table inet dstack_dante_leg)"
nft -D dante_leg=lo -f "$rules"
second="$(nft list table inet dstack_dante_leg)"
[ "$first" = "$second" ] || fail "a second load changed the table"
tables="$(nft list tables | grep -cx 'table inet dstack_dante_leg' || true)"
[ "$tables" = "1" ] || fail "expected one dstack_dante_leg table, got $tables"
grep -q 'iifname != "lo" accept' <<<"$second" || fail "the interface did not reach the in chain"
grep -q '29800-31847' <<<"$second" || fail "the dstack flow range is not in the set"

if nft -f "$rules" 2>/dev/null; then
    fail "a load without -D dante_leg=<iface> succeeded"
fi
after="$(nft list table inet dstack_dante_leg)"
[ "$after" = "$second" ] || fail "a failed load changed the table"

echo "dante-leg.nft: loads, reloads idempotently, refuses a load without the interface"

# The notrack table, with a dummy interface as the leg (192.0.2.0/24 is
# TEST-NET-1) and lo as the other NIC.
ip link set lo up
ip link add dl0 type dummy
ip addr add 192.0.2.1/24 dev dl0
ip link set dl0 up
nft -D dante_leg=dl0 -f "$rules"
nft -D dante_leg=dl0 -f "$notrack"
first="$(nft list table inet dstack_dante_leg_notrack)"
nft -D dante_leg=dl0 -f "$notrack"
second="$(nft list table inet dstack_dante_leg_notrack)"
[ "$first" = "$second" ] || fail "a second load changed the notrack table"
tables="$(nft list tables | grep -cx 'table inet dstack_dante_leg_notrack' || true)"
[ "$tables" = "1" ] || fail "expected one dstack_dante_leg_notrack table, got $tables"
grep -q 'iifname != "dl0" notrack' <<<"$second" || fail "the interface did not reach raw_pre"
grep -q 'oifname != "dl0" notrack' <<<"$second" || fail "the interface did not reach raw_out"
if nft -f "$notrack" 2>/dev/null; then
    fail "a load of the notrack table without -D dante_leg=<iface> succeeded"
fi
[ "$(nft list table inet dstack_dante_leg_notrack)" = "$second" ] ||
    fail "a failed load changed the notrack table"

# A test-only table counts what conntrack made of each packet, after it.
nft -f - <<'RULES'
table inet probe {
	counter lo_untracked {}
	counter lo_tracked {}
	counter leg_untracked {}
	counter leg_tracked {}
	chain out {
		type filter hook output priority filter + 10; policy accept;
		oifname "lo" ct state untracked counter name lo_untracked
		oifname "lo" ct state new,established counter name lo_tracked
		oifname "dl0" ct state untracked counter name leg_untracked
		oifname "dl0" ct state new,established counter name leg_tracked
	}
}
RULES
ping -c 2 -W 1 127.0.0.1 >/dev/null
# The dummy leg drops what it sends, so the ping gets no answer.
ping -c 2 -W 1 192.0.2.2 >/dev/null || true
packets() {
    nft list counter inet probe "$1" | sed -n 's/.*packets \([0-9]*\).*/\1/p'
}
[ "$(packets lo_untracked)" -ge 2 ] || fail "lo (not the leg) was not untracked"
[ "$(packets lo_tracked)" = "0" ] || fail "lo (not the leg) was tracked"
[ "$(packets leg_tracked)" -ge 2 ] || fail "the leg was not tracked"
[ "$(packets leg_untracked)" = "0" ] || fail "the leg was untracked"

echo "dante-leg-notrack.nft: loads, reloads idempotently, refuses a load without the interface, untracks only off the leg"

# The mDNS caps (dstack #288) on the dummy leg: the host's own mDNS
# answers to the group pass up to the burst (200) and 50/s, past that they
# are dropped and counted; queries (DNS QR bit 0) have a cap of their own,
# 10/s past a burst of 50, which never touches answers. Other UDP to the
# group and mDNS off the leg are never capped. A drop in the output hook
# fails the send (EPERM), so the sender counts what passed.
ip route add 224.0.0.0/4 dev dl0
nft -D dante_leg=dl0 -f "$rules"
send() { # <from address> <port> <datagrams> <query|answer> [per second; flat out without]: prints how many were sent
    python3 - "$@" <<'PY'
import socket, sys, time
src, port, n, kind = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
rate = float(sys.argv[5]) if len(sys.argv) > 5 else 0.0
# A DNS header (id 0, flags: QR=1 AA=1 for an answer, 0 for a query, no
# records), padded to 100 bytes.
flags = {"answer": b"\x84\x00", "query": b"\x00\x00"}[kind]
payload = (b"\0\0" + flags + b"\0" * 8).ljust(100, b"\0")
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind((src, 0))
s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, socket.inet_aton(src))
s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 0)
sent = 0
start = time.monotonic()
for i in range(n):
    if rate > 0:
        wait = start + i / rate - time.monotonic()
        if wait > 0:
            time.sleep(wait)
    try:
        s.sendto(payload, ("224.0.0.251", port))
        sent += 1
    except PermissionError:
        pass
print(sent)
PY
}
capped() {
    nft list chain inet dstack_dante_leg out |
        sed -n 's/.*limit rate over 50\/second burst 200 packets counter packets \([0-9]*\).*/\1/p'
}
queries_capped() {
    nft list chain inet dstack_dante_leg out |
        sed -n 's/.*limit rate over 10\/second burst 50 packets counter packets \([0-9]*\).*/\1/p'
}
[ "$(queries_capped)" = "0" ] || fail "the mDNS query cap rule is missing or counted before any send"
[ "$(capped)" = "0" ] || fail "the mDNS cap rule is missing or counted before any send"
[ "$(send 192.0.2.1 5353 20 answer)" = "20" ] || fail "normal mDNS announces on the leg were dropped"
[ "$(send 192.0.2.1 5353 5 query)" = "5" ] || fail "normal mDNS queries on the leg were dropped"
[ "$(queries_capped)" = "0" ] || fail "the mDNS query cap dropped normal queries"
[ "$(capped)" = "0" ] || fail "the mDNS cap dropped normal announces"

# A querier at 40/s for 6 s (#285's rate): the 45 burst tokens left, then
# 10/s, so about 105 of 240 pass.
queries="$(send 192.0.2.1 5353 240 query 40)"
[ "$queries" -ge 80 ] || fail "the query flood passed $queries of 240, under the burst and 10/s"
[ "$queries" -le 135 ] || fail "the query flood passed $queries of 240: queries not capped at 10/s"
[ "$(queries_capped)" = "$((240 - queries))" ] ||
    fail "the query cap's counter $(queries_capped) is not the $((240 - queries)) dropped"
[ "$(capped)" = "0" ] || fail "the 50/s cap dropped queries under it"
# The query cap is spent now; an answer burst of 30 in 100 ms passes whole.
[ "$(send 192.0.2.1 5353 30 answer 300)" = "30" ] || fail "an answer burst was dropped"
[ "$(queries_capped)" = "$((240 - queries))" ] || fail "the query cap counted answers"
[ "$(capped)" = "0" ] || fail "the 50/s cap dropped an answer burst of 30"

echo "dante-leg.nft: mDNS queries capped at 10/s ($queries of 240 at 40/s passed), an answer burst of 30 in 100 ms passed"

# The answer cap, from a fresh load (full buckets).
nft -D dante_leg=dl0 -f "$rules"
flood="$(send 192.0.2.1 5353 3000 answer)"
[ "$flood" -ge 180 ] || fail "the flood passed $flood mDNS datagrams, under the burst"
[ "$flood" -le 1000 ] || fail "the flood passed $flood of 3000 mDNS datagrams: not capped"
[ "$(capped)" = "$((3000 - flood))" ] || fail "the cap's counter $(capped) is not the $((3000 - flood)) dropped"
[ "$(queries_capped)" = "0" ] || fail "the query cap counted answers"
[ "$(send 192.0.2.1 5354 3000 query)" = "3000" ] || fail "UDP to the group on another port was capped"
# Another NIC (TEST-NET-2); the sender's IP_MULTICAST_IF picks it.
ip link add dl1 type dummy
ip addr add 198.51.100.1/24 dev dl1
ip link set dl1 up
[ "$(send 198.51.100.1 5353 3000 answer)" = "3000" ] || fail "mDNS answers off the leg were capped"
[ "$(send 198.51.100.1 5353 3000 query)" = "3000" ] || fail "mDNS queries off the leg were capped"
sleep 1
[ "$(send 192.0.2.1 5353 5 answer)" = "5" ] || fail "mDNS announces a second after a flood were dropped"

echo "dante-leg.nft: the leg's own mDNS out is capped (flood dropped and counted), announces and other traffic pass"
