#!/usr/bin/env bash
set -euo pipefail
# SPDX-License-Identifier: MIT OR Apache-2.0
# dstack #411, #414, #451 (the whole-device proof of #409, #411, #414 and
# #451, in CI):
# a NIC destroyed and created again under the same name and address, with
# a new interface index (stream-snv 18.11.2026: a USB reset of the Dante
# NIC), must leave a running device hearing its transmitter again, without
# a restart.
#
# Two private network namespaces (nothing of the runner's own network is
# touched), one `dstack-dev peer` in each, joined by a veth pair named rig0
# at both ends: a on 10.71.0.1/24, b on 10.71.0.2/24, each receiving the
# other's tone. Peer b starts with a multicast flow of its TX 1 (a saved
# one, its state file's [[multicast]], as a controller would have made), so
# a receives b by MULTICAST (our own multicast TX flow socket, #411) and b
# receives a by unicast (a's flow request and the directory's queries,
# #409). Both run as root, so each opens the AF_PACKET socket past the
# qdisc (`media::bypass`, CAP_NET_RAW), and a's unicast flow to b, a
# resolved neighbour on its own link, goes past the qdisc (#414): a's
# packets sent direct must rise too. Once both deliver, the veth pair is
# deleted, left gone GONE_S seconds and added again (a's rig0 index must
# differ); then within BOUND_S each must deliver again: its packets from
# the other rose by MIN_RISE in a 2 s window (about 1.3 s of audio at 48
# kHz, 32 frames a packet), a's still by multicast, a's flow to b still
# direct, b's log names its flow sockets' send interface again and a's log
# its direct socket's new index. And a's mDNS daemon must answer again
# (#451): from b's namespace, a direct query to a's address for a's own
# ARC record (`<name>._netaudio-arc._udp.local.` SRV), answered once, from
# a's address, within BOUND_S of the re-add (asked before the delete too,
# so a missing answer after it is the re-create's).
# The reads are recreate_gate.py's (unit tests: recreate_gate_test.py).
# Run as root; the binary is $1 (default target/release/dstack-dev).

BIN=$(realpath "${1:-target/release/dstack-dev}")
GATE="$(dirname "$(realpath "$0")")/recreate_gate.py"
DIR=$(mktemp -d)
NA="dstack-recreate-a-$$"
NB="dstack-recreate-b-$$"
IF=rig0
ADDR_A=10.71.0.1/24
ADDR_B=10.71.0.2/24
IP_A=${ADDR_A%/*}
A=recreate-a
B=recreate-b
GONE_S=5
BOUND_S=60
MIN_RISE=2000
STOP_S=10
ASK_S=1

# Stops the peers still running: INT (their goodbyes), KILL after STOP_S;
# false when one needed the KILL. `ip netns del` kills nothing inside.
stop_peers() {
  local pid alive _
  for pid in ${PIDS:-}; do kill -INT "$pid" 2>/dev/null || true; done
  for _ in $(seq 1 $((STOP_S * 10))); do
    alive=""
    for pid in ${PIDS:-}; do kill -0 "$pid" 2>/dev/null && alive="$alive $pid"; done
    [ -n "$alive" ] || return 0
    sleep 0.1
  done
  for pid in $alive; do kill -KILL "$pid" 2>/dev/null || true; done
  return 1
}
cleanup() {
  stop_peers || true
  ip netns del "$NA" 2>/dev/null || true
  ip netns del "$NB" 2>/dev/null || true
  rm -rf "$DIR"
}
trap cleanup EXIT

# One end of the veth pair in each namespace, both named rig0: deleting
# either end deletes both, as a NIC's reset takes the link away.
leg_up() {
  ip -n "$NA" link add "$IF" type veth peer name "$IF" netns "$NB"
  ip -n "$NA" addr add "$ADDR_A" dev "$IF"
  ip -n "$NB" addr add "$ADDR_B" dev "$IF"
  local ns
  for ns in "$NA" "$NB"; do
    ip -n "$ns" link set "$IF" up multicast on
    ip -n "$ns" route add 224.0.0.0/4 dev "$IF"
  done
}
index() { ip -n "$NA" -o link show "$IF" | python3 -I "$GATE" index "$IF"; }

ip netns add "$NA"
ip netns add "$NB"
ip -n "$NA" link set lo up
ip -n "$NB" link set lo up
leg_up

cat >"$DIR/b-state.toml" <<EOF
name = "$B"
rx = ["RX 1"]
tx = ["TX 1"]

[[multicast]]
slot = 0
channels = [1]
EOF

# `ip netns exec` execs the peer, so `$!` is the peer itself and the INT at
# the end reaches it, not a subshell.
ip netns exec "$NA" "$BIN" peer --iface "$IF" --name "$A" --tone-hz 1000 \
  --rx-from "$B:TX 1" --test-clock-ppm 6 \
  --state "$DIR/a-state.toml" --status "$DIR/a.json" >"$DIR/a.out" 2>"$DIR/a.log" &
PA=$!
ip netns exec "$NB" "$BIN" peer --iface "$IF" --name "$B" --tone-hz 440 \
  --rx-from "$A:TX 1" --test-clock-ppm 6 \
  --ports 4441,8801,4456 --process-id 1 \
  --state "$DIR/b-state.toml" --status "$DIR/b.json" >"$DIR/b.out" 2>"$DIR/b.log" &
PB=$!
PIDS="$PA $PB"

fail() {
  echo "FAIL: $*" >&2
  for p in a b; do
    echo "--- $p.json"; cat "$DIR/$p.json" 2>/dev/null || true; echo
    echo "--- $p.log"; tail -n 60 "$DIR/$p.log" || true
  done
  exit 1
}

# `<packets> <multicast>`: a's flow from b, then b's from a (a missing
# line reads as unread, -1); then a's packets sent past the qdisc.
heard() {
  python3 -I "$GATE" flow "$DIR/a.json" "$B"
  python3 -I "$GATE" flow "$DIR/b.json" "$A"
  python3 -I "$GATE" direct "$DIR/a.json"
}
# Whether both deliver over one 2 s window, a's from b by multicast and
# a's to b past the qdisc. Sets HEARD to the window's counts.
both_deliver() {
  local a0 b0 d0 a1 am b1 d1
  { read -r a0 _; read -r b0 _; read -r d0; } < <(heard)
  sleep 2
  { read -r a1 am; read -r b1 _; read -r d1; } < <(heard)
  a0=${a0:--1} b0=${b0:--1} d0=${d0:--1} a1=${a1:--1} am=${am:--1} b1=${b1:--1} d1=${d1:--1}
  HEARD="a from b: $a0 -> $a1 (multicast $am); b from a: $b0 -> $b1 (a direct $d0 -> $d1)"
  python3 -I "$GATE" rose "$a0" "$a1" "$MIN_RISE" &&
    python3 -I "$GATE" rose "$b0" "$b1" "$MIN_RISE" &&
    python3 -I "$GATE" rose "$d0" "$d1" "$MIN_RISE" &&
    [ "$am" = 1 ]
}
# Waits up to $1 s for both to deliver (WAITED: the seconds it took), or
# fails with $2.
until_both_deliver() {
  local start=$SECONDS
  until both_deliver; do
    [ $((SECONDS - start)) -lt "$1" ] || fail "$2 in $1 s ($HEARD)"
    kill -0 "$PA" 2>/dev/null || fail "peer a ended"
    kill -0 "$PB" 2>/dev/null || fail "peer b ended"
  done
  echo "$HEARD"
  WAITED=$((SECONDS - start))
}

# Waits until a answers a direct query for its own ARC record from b's
# namespace, once and from its address (`recreate_gate.py ask`), at most
# $1 s after the SECONDS mark $2 (ANSWERED: the seconds since the mark),
# or fails with $3.
until_a_answers() {
  local out
  until out=$(ip netns exec "$NB" python3 -I "$GATE" ask "$IP_A" "$A._netaudio-arc._udp.local." "$ASK_S"); do
    [ $((SECONDS - $2)) -lt "$1" ] || fail "$3 in $1 s ($out)"
    kill -0 "$PA" 2>/dev/null || fail "peer a ended"
  done
  echo "$out"
  ANSWERED=$((SECONDS - $2))
}

until_both_deliver 60 "the peers did not deliver to each other (a from b by multicast, a to b past the qdisc)"
echo "before: both deliver ($WAITED s)"
until_a_answers 60 "$SECONDS" "a never answered a direct query for its own ARC record"
echo "before: a answers its own ARC record"

OLD=$(index)
ip -n "$NA" link del "$IF"
sleep "$GONE_S"
leg_up
READD=$SECONDS
NEW=$(index)
[ "$OLD" != "$NEW" ] || fail "$IF came back on its old index $OLD: nothing re-created"
echo "$IF re-created: a's index $OLD -> $NEW, gone $GONE_S s"

until_both_deliver "$BOUND_S" "not heard again after the re-create"
echo "after: both deliver again, the delivering window ended $WAITED s after the re-add"
until_a_answers "$BOUND_S" "$READD" "a did not answer its own ARC record again after the re-create"
echo "after: a answers its own ARC record again, $ANSWERED s after the re-add"

grep -F "multicast send interface named again" "$DIR/b.log" | grep -qF "the TX flow sockets" ||
  fail "b's log never named its flow sockets' send interface again"
grep -qF "the direct socket sends on the interface's new index" "$DIR/a.log" ||
  fail "a's log never moved its direct socket to the new index"

stop_peers || fail "a peer did not stop within $STOP_S s of its INT"
PIDS=""
wait "$PA" || fail "peer a exited $?"
wait "$PB" || fail "peer b exited $?"
echo "recreate: a hears b by multicast, b hears a past the qdisc and a answers its ARC record again after $IF's re-create"
