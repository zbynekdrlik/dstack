---
paths:
  - "tools/windows/**"
  - "docs/two-networks.md"
  - "docs/windows-box.md"
---

# Windows PCs: network legs and the tools for them

- **One NIC stays** (owner 12.11.2026, #239): never propose two NICs again. The vendor VLAN tool (Realtek `8169DiagV2.exe`) never stays running after setup; close it without clicking its dialogs. A Realtek VLAN adapter's MAC changes on a USB replug and on every base-adapter restart.
- **Every one-NIC PC gets the LAN self-heal task** (`tools/windows/lan-selfheal.ps1 -Mode install`, docs/two-networks.md section 3). The VLAN property is found by its display name "VLAN ID": its keyword is the driver's (`RegVlanID` on the RTL8156 base adapter, `VlanID` on the Realtek Virtual Adapter). The config's `lanVlanId` (required since 0.6.105) states the LAN adapter's VLAN: 0 when the LAN is the untagged base adapter, N when the LAN is a VLAN adapter (a dasio host keeps Dante untagged on the base adapter); a script change that adds a required config key also migrates every installed config.
- **A LAN-down test** (disabling the LAN leg on purpose) is a separate run the owner times; installing and checking "gateway ok" is not.
- **Testing a PowerShell tool:** dev1 has no `pwsh`. CI's `windows-tools` job runs `tools/windows/tests/*.Tests.ps1` under Windows PowerShell 5.1 (plain asserts, no Pester: inbox 3.4 and runner 5.x differ). Before the push, push the commit to a new `refs/autopilot-wip/<branch>-<topic>` ref per commit (a force push to an existing wip ref is hook-blocked) and fetch the files on a Windows PC by SHA from `raw.githubusercontent.com/zbynekdrlik/dstack/<sha>/...` (check `Get-FileHash`), then run the test there: no 20 KB paste through MCP.
- **PowerShell strings:** `"$var: text"` is a parse error (`:` after a variable reads as a scope); write `"${var}: text"`. A function whose value is an exit code must not leak cmdlet output into it: take the last value (`$rc | Select-Object -Last 1`). Under `Set-StrictMode -Version 2` a function that returns an empty array gives `$null`, and `$null.Count` throws: count with `@(f).Count`.
- **Testing adapter changes without an adapter:** define functions named like the NetAdapter/NetTCPIP cmdlets in the test script after dot-sourcing the tool (a function wins over a cmdlet) and record each call's target; the heal path then runs on CI with no NIC touched (`tools/windows/tests/lan-selfheal.Tests.ps1`).
- **dasio's PCs** (its test PC, FOH, its Dante Controller PC): dstack does no Windows OS work there; it gives the tool and the plan on the ticket.
