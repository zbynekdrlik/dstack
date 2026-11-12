---
paths:
  - "crates/dstack/src/arc.rs"
  - "crates/dstack/src/arc/**"
  - "crates/dstack/src/announce.rs"
  - "crates/dstack/tests/fixtures/arc_*.json"
---

# ARC and Dante Controller

- **What Dante Controller (DC) decides from what we answer** (#216, Findings 16-18): the protocol comes from the ARC TXT (`arcp_vers`). A 2.8.9 device gets start code 0x2809 for every read. 0x1000's first word then picks the features:
  - 0x0001: the device rename (2.8.9 only; under 2.7 the field stays greyed);
  - 0x0010: TX channel rename;
  - 0x0020: multicast TX flows (Transmit tab "Add"). With 0x1000 clear DC creates with 2.7's 0x2201 (we refuse it: no flow); with 0x1000 set, with 2.8.9's 0x2601 (#220). Kept clear.
  - 0x1000: segmented flows. With it DC reads 0x2400/0x3400/0x2600/0x3600 instead of the 2.7 lists and writes with 0x3410/0x3401/0x2438 (#220). It lists our device while our 0x2600 page is empty; once we list any TX flow there, DC cannot read it and re-reads 0x1000…0x2600 without end (our records are #148's `0c 17` chain, the card's are `14 27` records). Kept clear until 0x2600 takes the card's layout.
  - 0x0d00 (a card's high bits) also hid our device.
  - Board-info flag byte 0x17 bit 0x02 enables Reboot/Factory Reset.
  - With 0x1000 clear DC routes with 0x3010 and renames RX channels with 0x3001. DC puts a TX channel back to its default with a 2.8.9 name offset of 0.
- **netaudio 0.4.2** (installed under `/opt/pipx/venvs/netaudio`, Rust core `libnetaudio_core.so`): it picks subscriptions by `arcp_vers` (0x3410 on a 2.8.9 device) and the channel-name opcode by probing 0x3400/0x2400 under 0x2809 (an answer means 0x3401 for RX; TX keeps 0x2013 under 0x2809; its 0x2438 is a preset reconcile the CLI never sends). It refuses a self-subscription unless the RX record's flags carry 0x0008. Its reset puts the name offset at the datagram's end, no NUL. Encode a fixture frame offline with `/opt/pipx/venvs/netaudio/bin/python -I` and `netaudio.core.build_command(spec)` / `plan_subscription_commands` (a channel needs `media_type_code` 3); the Rust source is a protocol reference only. `subscription list` and `flow inspect` probe every device on the network (reads); for our device's state use `channel list --name <ours>` and our ctl `status`.
- **One variable per DC session.** DC reads a device once, at its start: restart DC for every value tried. The quick loop is a scratch build that reads the value from an environment variable. Copy the binary out, then `git checkout -- <file>`; never commit it. Read DC's requests from our device's ctl `status` (`requests`: peer, port, op, n) or from pktmon (`C:\dstack-tools\arcdump216.py`, `udpdump216.py` on mbc).
- **DC GUI on mbc:** check that the Rename field is editable with a triple-click into it, and look for the selection highlight (zoom) before typing. Text typed while the field is greyed goes to Device View's device selector and switches the view to another device (read only, but it is another device: switch back from the selector). Close DC with `CloseMainWindow`, then `Stop-Process`. A second `DanteController` process without a window can stay behind: stop it too before the next session. Launch DC with the MCP `App` tool, wait ~30 s, read its window handle from `Get-Process DanteController` and `FocusWindow` it (Ableton is in front on mbc). Device View opens a few seconds after the double-click on our device's row. A channel rename: double-click the name, `ctrl+a`, zoom to see it selected, type, Enter. A subscription: expand our device in Available Channels and drag a TX channel onto the RX row. `C:\dstack-tools\udpdump220.py <pcapng> <ip> <bytes>` prints whole datagrams.
- **A real device's replies carry stale buffer bytes:** after a string's NUL, in unused page-table slots, and in fields it does not fill (the card's 0x1003 bytes 20-27 held the previous reply's bytes). Compare two reads before taking a byte for a field. Zero name fragments in a fixture, and say which stale bytes are kept.
- **A worktree lane's long runs** (full `cargo test`, the mutation gate): the isolation checker refuses `setsid ... &` around a cargo command line. Write a small script (`set -euo pipefail`, output to a log, `RC=` at the end), start it with `setsid nohup bash <script> &`, and wait on its pid with `timeout 170 tail --pid=<pid> -f /dev/null`.
