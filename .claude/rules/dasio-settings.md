---
paths:
  - "crates/dtray/src/settings_win.rs"
  - "crates/dtray/src/settings_win/**"
  - "crates/dtray/src/dasio.rs"
  - "crates/dtray/src/dasio/**"
---

# dtray's dasio settings window (#246, #512)

- **Size budget:** the window is DPI unaware (fixed pixels, Windows scales it), so its logical work area is the screen / scale less the taskbar: 1366 x 768 at 125 % is about 1093 x 566, 1920 x 1200 at 125 % about 1536 x 912. Since #512 the client area is 1040 x 514 (outer 1056 x 553), placed by `dasio::window::place` inside `SPI_GETWORKAREA`; every tab has the same size (a bridge instance's group takes the latency group's place, never grows the window). A new row goes into the layout constants (`settings_win.rs`: `LABEL_X`, `CTL_X`, `HINT_X`, `LOWER_Y`, `HALF_W`, `TOLD_*`; `bridge.rs` `BELOW`), never below the buttons.
- **Control ids** (`settings_win.rs`): `ID_RESTART..+RESTARTS` (13 ↻ markers), `ID_HINT..+6` (grey hints, coloured in `paint.rs`), `ID_BRIDGE_LABEL..+6` at 190. A new marker or hint extends its range constant and the colour range in the same commit.
- **The latency the DAW is told** is dasio's formula (`crates/dasio/src/lib.rs` `latencies`): input = B + P, output = B + W + S x B + T, T = `tx_latency_ns` else `latency_ns`, P and T `dstack::clock::latency_samples`. It lives in `dasio::told` with dasio's own test vectors; a bridge instance gets no sum (its status has no `latency_in`/`latency_out`, `bridge_json`). If dasio changes `latencies`, change `told::Terms` with the same vector.
- **Checks before Apply are pure** (`dasio::check`): the least app reserve (one block; 2 ms under `timing = "normal"`, `Fields::normal`), the reserve after a buffer pick, and the config's own refusal of the file (`file_problem`). The glue only calls them.
- **Texts:** each ms term rounds on its own, so a sum line carries the samples too (`(64 + 160 = 224 samples)`); compare with dasio's app line (`in X / out Y ms`) in samples, not by adding the rounded ms.
- **Checked on mbc** (v0.6.232, 28.11.2026): dtray in `C:\Program Files\dasio` replaced with the released exe (sha256 checked; the old one renamed `dtray-v0.6.231.exe.bak`, the running tray stopped by pid and started again), `vmon.ps1 in`, `dtray.exe --settings` via `Start-Process`, Snapshot at `max_width: 0`: the window 1056 x 553, every row named with its unit, the told sums (input 736, output 1248 samples) equal to dasio's app line (`in 7.67 / out 13.00 ms`). Close with Cancel, then `vmon.ps1 out`.
