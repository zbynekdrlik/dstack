#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# dstack #305: docs/ci-peer.md's recipe, as an application's CI runs it: a
# dummy interface, two `dstack-dev peer` devices on its one address (each
# on the TEST system clock with its own skew), each receiving the other's
# tone. Gated as the doc says: after a settle, no tone gap, no RX hole, no
# pump restart, no play-time step, and each peer's play_at_ppm at its skew.
# Run as root (the interface); the binary is $1 (default target/release).
set -euo pipefail

BIN=${1:-target/release/dstack-dev}
DIR=$(mktemp -d)
IF=dante0

cleanup() {
  for pid in ${PIDS:-}; do kill -INT "$pid" 2>/dev/null || true; done
  ip link del "$IF" 2>/dev/null || true
}
trap cleanup EXIT

ip link add "$IF" type dummy
ip addr add 10.70.0.1/24 dev "$IF"
ip link set "$IF" up multicast on
ip route add 224.0.0.0/4 dev "$IF"

"$BIN" peer --iface "$IF" --name ci-peer-a --tone-hz 1000 \
  --rx-from "ci-peer-b:TX 1" --test-clock-ppm 6 \
  --status "$DIR/a.json" 2>"$DIR/a.log" &
A=$!
"$BIN" peer --iface "$IF" --name ci-peer-b --tone-hz 440 \
  --rx-from "ci-peer-a:TX 1" --test-clock-ppm -6 \
  --ports 4441,8801,4456 --process-id 1 \
  --status "$DIR/b.json" 2>"$DIR/b.log" &
B=$!
PIDS="$A $B"

fail() {
  echo "FAIL: $*" >&2
  for p in a b; do
    echo "--- $p.json"; cat "$DIR/$p.json" 2>/dev/null || true; echo
    echo "--- $p.log"; tail -n 40 "$DIR/$p.log" || true
  done
  exit 1
}

locked() { jq -e '.peer.rx[0].locked' "$DIR/$1.json" >/dev/null 2>&1; }
for _ in $(seq 1 60); do
  if locked a && locked b; then break; fi
  sleep 1
done
locked a && locked b || fail "the peers did not lock on each other's tone in 60 s"

# The counts a gate takes the rise of.
counts() {
  jq -c '[.peer.rx[0].tone_gaps,
          ([.device.channels[] | .holes + .late] | add // 0),
          ([.device.rx[] | .gaps + .late + .socket_drops] | add // 0),
          .device.engine.rebootstraps]' "$DIR/$1.json"
}
sleep 5
A0=$(counts a); B0=$(counts b)
sleep 20
A1=$(counts a); B1=$(counts b)
echo "a: $A0 -> $A1; b: $B0 -> $B1"
[ "$A0" = "$A1" ] || fail "peer a: [tone_gaps, channel holes+late, flow gaps+late+drops, rebootstraps] rose $A0 -> $A1"
[ "$B0" = "$B1" ] || fail "peer b: [tone_gaps, channel holes+late, flow gaps+late+drops, rebootstraps] rose $B0 -> $B1"
for p in a b; do
  jq -e '.device.engine.play_at_steps == 0' "$DIR/$p.json" >/dev/null || fail "$p: play_at_steps"
  jq -e '((.peer.play_at_ppm - .peer.test_clock_ppm) | fabs) < 2' "$DIR/$p.json" >/dev/null \
    || fail "$p: play_at_ppm off its test_clock_ppm"
  jq -e '.device.clock.source == "system"' "$DIR/$p.json" >/dev/null || fail "$p: clock source"
done
jq -e '(.peer.rx[0].tone_hz - 440 | fabs) < 0.1' "$DIR/a.json" >/dev/null || fail "a heard no 440 Hz"
jq -e '(.peer.rx[0].tone_hz - 1000 | fabs) < 0.1' "$DIR/b.json" >/dev/null || fail "b heard no 1000 Hz"

PIDS=""
kill -INT "$A" "$B"
wait "$A" || fail "peer a exited $?"
wait "$B" || fail "peer b exited $?"
echo "ci-peer: both peers clean ($(jq -c '.peer.rx[0] | {tone_hz, tone_gaps}' "$DIR/a.json"), \
$(jq -c '.peer.rx[0] | {tone_hz, tone_gaps}' "$DIR/b.json"))"
