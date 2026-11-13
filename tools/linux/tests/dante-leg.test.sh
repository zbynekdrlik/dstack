#!/bin/bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# The generic Dante-leg filter (tools/linux/dante-leg.nft, dstack #264)
# loaded in a private network namespace (run as root, CI's linux-tools
# job): it loads, a second load replaces it (one table, the same rules), a
# load without the interface fails and leaves the loaded table as it was.
# The port sets themselves are checked by crates/dstack/tests/dante_leg.rs.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
rules="$here/../dante-leg.nft"

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
tables="$(nft list tables | grep -c 'dstack_dante_leg' || true)"
[ "$tables" = "1" ] || fail "expected one dstack_dante_leg table, got $tables"
grep -q 'iifname != "lo" accept' <<<"$second" || fail "the interface did not reach the in chain"
grep -q '29800-31847' <<<"$second" || fail "the dstack flow range is not in the set"

if nft -f "$rules" 2>/dev/null; then
    fail "a load without -D dante_leg=<iface> succeeded"
fi
after="$(nft list table inet dstack_dante_leg)"
[ "$after" = "$second" ] || fail "a failed load changed the table"

echo "dante-leg.nft: loads, reloads idempotently, refuses a load without the interface"
