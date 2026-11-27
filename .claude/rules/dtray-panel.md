---
paths:
  - "crates/dtray/src/panel.rs"
  - "crates/dtray/src/panel/**"
  - "crates/dtray/src/panel_win.rs"
  - "crates/dtray/src/tray.rs"
  - "crates/dtray/src/win.rs"
---

# dtray's panel (Windows, #507)

- **Never put an operator control in the native menu's submenus again.** tray-icon/muda's popup menu is Windows' `TrackPopupMenu`: its width is the longest item text and a submenu is placed against the MONITOR, not the work area. On mbc (1920x1200 at 125 %, collapsed tablet taskbar) the menu was 1156 px wide and dlisten's Output submenu ran under the taskbar, its checked item cut (27.11.2026). The left click opens the panel (`panel_win.rs`); the right click keeps the menu.
- **Geometry and rows are pure** (`dtray::panel`: `place`, `rows`, `output_row`, `scroll_to`), tested with small and offset work areas; `panel_win.rs` is glue, excluded from the mutation gate. A new decision goes into `panel.rs` with a test, never into the glue.
- **DPI:** the panel alone is per-monitor v2: `Pmv2::enter()` around every call that creates, measures or places it (and around `TrayIcon::rect()`, whose `Shell_NotifyIconGetRect` answers in the calling thread's DPI). The rest of dtray stays DPI unaware (the dasio settings window lays out in fixed pixels). A move onto another monitor's DPI is caught in `layout` by `GetDpiForWindow`, since `WM_DPICHANGED` arrives while the state is borrowed. The anchor falls back to the cursor point when the icon rect does not hold the cursor.
- **The picker applies on `CBN_SELENDOK` only**, never `CBN_SELCHANGE` (arrow keys in the open list would move dlisten's output at every step), and the wheel over the panel or its controls is taken in `dialog_message` before a closed combobox sees it (Windows sends the wheel to the control under the cursor). Each tick resets a picker's selection to what the app reports, and skips the whole refresh while any list is open; a combobox is SIZED once at creation and only MOVED after (`SWP_NOSIZE`): a resize closes an open drop-down list, and the 1 s refresh would close it under the user's click.
- **Warnings come from dlisten's status:** `waiting_for` (plays nothing), no `output` (nothing open), and `pinned` (the `--output` text) vs the output playing, compared on the RAW status texts (`app_line` cuts to 64 for display only). `waiting_for` is the run's CURRENT name: after a pick in dtray it is the picked output, not `--output` (`session.rs` `choose_output`), so the text says which, by dlisten's own rule (any case, the name holds the text: `follow::matching`).
- **Seeing it on mbc:** the taskbar is the collapsed tablet form; `Win+B` shows the tray, then `ElementTree`/`Snapshot` lists the icon ("dstack: N devices, ..."). dtray lives in `C:\Program Files\dasio\dtray.exe` (HKLM Run, dasio installs it); on 27.11.2026 it was not running since the boot. Read and click only on mbc; never during an event (the SNV service from 17:00 CET).
