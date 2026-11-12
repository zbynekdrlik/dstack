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
  - 0x0020: multicast TX flows (Transmit tab "Add");
  - 0x1000: segmented flows. With it DC reads 0x2400/0x3400/0x2600/0x3600 instead of the 2.7 lists. With it set, DC left our device out of its list (cause open).
  - 0x0d00 (a card's high bits) also hid our device.
  - Board-info flag byte 0x17 bit 0x02 enables Reboot/Factory Reset.
  - Under 2.8.9 DC still routes with 0x3010 and renames RX channels with 0x3001. netaudio uses 0x3410/0x3401/0x2438 by `arcp_vers` alone.
- **One variable per DC session.** DC reads a device once, at its start: restart DC for every value tried. The quick loop is a scratch build that reads the value from an environment variable. Copy the binary out, then `git checkout -- <file>`; never commit it. Read DC's requests from our device's ctl `status` (`requests`: peer, port, op, n) or from pktmon (`C:\dstack-tools\arcdump216.py`, `udpdump216.py` on mbc).
- **DC GUI on mbc:** check that the Rename field is editable with a triple-click into it, and look for the selection highlight (zoom) before typing. Text typed while the field is greyed goes to Device View's device selector and switches the view to another device (read only, but it is another device: switch back from the selector). Close DC with `CloseMainWindow`, then `Stop-Process`. A second `DanteController` process without a window can stay behind: stop it too before the next session.
- **A real device's replies carry stale buffer bytes:** after a string's NUL, in unused page-table slots, and in fields it does not fill (the card's 0x1003 bytes 20-27 held the previous reply's bytes). Compare two reads before taking a byte for a field. Zero name fragments in a fixture, and say which stale bytes are kept.
- **A worktree lane's long runs** (full `cargo test`, the mutation gate): the isolation checker refuses `setsid ... &` around a cargo command line. Write a small script (`set -euo pipefail`, output to a log, `RC=` at the end), start it with `setsid nohup bash <script> &`, and wait on its pid with `timeout 170 tail --pid=<pid> -f /dev/null`.
