---
paths:
  - "tools/linux/**"
  - "tools/windows/dante-leg.ps1"
  - "crates/dstack/tests/dante_leg.rs"
  - "docs/two-networks.md"
---

# Host filters (dante-leg.nft, dante-leg-notrack.nft, dante-leg.ps1)

- **notrack breaks NAT and stateful firewalls.** An untracked packet is never NATed, and a stateful rule on another NIC sees its own replies as untracked, not established. So raw `notrack` chains never go into `dante-leg.nft`, which loads on every Linux PC. They live in `dante-leg-notrack.nft`, loaded only on a PC with no Docker, libvirt, masquerade, ufw or firewalld (#264, v0.6.119). Docker runs on the presenter PC and dev1.
- **`oifname` at raw priority in the output hook is reliable.** `__ip_local_out` / `__ip6_local_out` pass the route's device to `NF_INET_LOCAL_OUT`, so no `fib` lookup is needed.
- **Syntax check without root on dev1 fails.** `nft -c` needs netlink, and `unshare -rn` is refused (`apparmor_restrict_unprivileged_userns=1`). Instead, pipe the file to `sudo -n nft -c -D dante_leg=lo -f -` on the presenter PC through its remoteos Shell. `-c` changes nothing, and stdin leaves no file. A load without `-D` fails with "unknown identifier".
- **CI loads the rulesets for real.** `tools/linux/tests/dante-leg.test.sh` runs as root in a private netns (linux-tools job). A new table gets its load, reload, missing-`-D` and behaviour checks there. Behaviour is checked with a test-only counter table and a dummy leg on TEST-NET-1. A `grep -c` on a table name must be exact (`-cx 'table inet …'`), or a sibling table matches too.
