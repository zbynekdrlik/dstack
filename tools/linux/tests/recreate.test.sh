#!/usr/bin/env bash
set -euo pipefail
# SPDX-License-Identifier: MIT OR Apache-2.0
# dstack #411 (the whole-device proof of #409 and #411, in CI): a NIC
# destroyed and created again under the same name and address, with a new
# interface index (stream-snv 18.11.2026: a USB reset of the Dante NIC),
# must leave a running device hearing its transmitter again, without a
# restart.
#
# In a private network namespace (nothing of the runner's own network is
# touched): a dummy interface rig0 carrying 10.71.0.1/24, two
# `dstack-dev peer` devices on it, each receiving the other's tone. Peer b
# starts with a multicast flow of its TX 1 (a saved one, its state file's
# [[multicast]], as a controller would have made), so a receives b by
# MULTICAST (our own multicast TX flow socket, #411) and b receives a by
# unicast (a's flow request and the directory's queries, #409). Once both
# deliver, rig0 is deleted, left gone GONE_S seconds and added again (its
# index must differ); then within BOUND_S each must deliver again: its
# packets from the other rose by MIN_RISE in a 2 s window (about 1.3 s of
# audio at 48 kHz, 32 frames a packet), a's still by multicast, and b's
# log names its flow sockets' send interface again.
# The reads are recreate_gate.py's (unit tests: recreate_gate_test.py).
# Run as root; the binary is $1 (default target/release/dstack-dev).

BIN=$(realpath "${1:-target/release/dstack-dev}")
GATE="$(dirname "$(realpath "$0")")/recreate_gate.py"
DIR=$(mktemp -d)
NS="dstack-recreate-$$"
IF=rig0
ADDR=10.71.0.1/24
A=recreate-a
B=recreate-b
GONE_S=5
BOUND_S=60
MIN_RISE=2000
STOP_S=10

in_ns() { ip netns exec "$NS" "$@"; }
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
  ip netns del "$NS" 2>/dev/null || true
  rm -rf "$DIR"
}
trap cleanup EXIT

leg_up() {
  in_ns ip link add "$IF" type dummy
  in_ns ip addr add "$ADDR" dev "$IF"
  in_ns ip link set "$IF" up multicast on
  in_ns ip route add 224.0.0.0/4 dev "$IF"
}
index() { in_ns ip -o link show "$IF" | python3 -I "$GATE" index "$IF"; }

ip netns add "$NS"
in_ns ip link set lo up
leg_up

cat >"$DIR/b-state.toml" <<EOF
name = "$B"
rx = ["RX 1"]
tx = ["TX 1"]

[[multicast]]
slot = 0
channels = [1]
EOF

# Not through in_ns: `$!` must be the peer itself (ip netns exec execs
# it), so the INT at the end reaches it, not a subshell.
ip netns exec "$NS" "$BIN" peer --iface "$IF" --name "$A" --tone-hz 1000 \
  --rx-from "$B:TX 1" --test-clock-ppm 6 \
  --state "$DIR/a-state.toml" --status "$DIR/a.json" >"$DIR/a.out" 2>"$DIR/a.log" &
PA=$!
ip netns exec "$NS" "$BIN" peer --iface "$IF" --name "$B" --tone-hz 440 \
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
# line reads as unread, -1).
heard() {
  python3 -I "$GATE" flow "$DIR/a.json" "$B"
  python3 -I "$GATE" flow "$DIR/b.json" "$A"
}
# Whether both deliver over one 2 s window, a's from b by multicast. Sets
# HEARD to the window's counts.
both_deliver() {
  local a0 b0 a1 am b1
  { read -r a0 _; read -r b0 _; } < <(heard)
  sleep 2
  { read -r a1 am; read -r b1 _; } < <(heard)
  a0=${a0:--1} b0=${b0:--1} a1=${a1:--1} am=${am:--1} b1=${b1:--1}
  HEARD="a from b: $a0 -> $a1 (multicast $am); b from a: $b0 -> $b1"
  python3 -I "$GATE" rose "$a0" "$a1" "$MIN_RISE" &&
    python3 -I "$GATE" rose "$b0" "$b1" "$MIN_RISE" &&
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

until_both_deliver 60 "the peers did not deliver to each other (a from b by multicast)"
echo "before: both deliver ($WAITED s)"

OLD=$(index)
in_ns ip link del "$IF"
sleep "$GONE_S"
leg_up
NEW=$(index)
[ "$OLD" != "$NEW" ] || fail "$IF came back on its old index $OLD: nothing re-created"
echo "$IF re-created: index $OLD -> $NEW, gone $GONE_S s"

until_both_deliver "$BOUND_S" "not heard again after the re-create"
echo "after: both deliver again, the delivering window ended $WAITED s after the re-add"

grep -F "multicast send interface named again" "$DIR/b.log" | grep -qF "the TX flow sockets" ||
  fail "b's log never named its flow sockets' send interface again"

stop_peers || fail "a peer did not stop within $STOP_S s of its INT"
PIDS=""
wait "$PA" || fail "peer a exited $?"
wait "$PB" || fail "peer b exited $?"
echo "recreate: a hears b by multicast and b hears a again after $IF's re-create"
