---
paths:
  - "crates/dlisten/**"
  - "crates/dstack-endpoint/src/outputs.rs"
  - "crates/dstack-endpoint/src/logfile.rs"
  - "crates/dstack-endpoint/src/host.rs"
---

# dlisten's output and its log

- **`--output` is a pin, not a start hint** (#506, v0.6.228): a named run plays on a matching output only (exact name, any case, first; then one that holds the text). Missing = nothing plays, the device runs, WARN once + an INFO line a minute, opened when the 5 s list read finds it. Never bring back a fallback to the default for a named run: on 26.11.2026 the FOH notebook's Headphones endpoint went UNPLUGGED for 3 min 19 s (Realtek jack detection, the plug stayed in), the old fallback moved SOLO to the room speakers for the rest of the service. The decision is `crates/dlisten/src/follow.rs` (pure, tested); `session.rs` is glue outside the mutation gate. Without `--output` the old behaviour stays.
- **cpal lists ACTIVE endpoints only** (`cpal-0.18.2/src/host/wasapi/device.rs`, `DEVICE_STATE_ACTIVE`): Windows' NOT PRESENT duplicates with the same name (`Get-PnpDevice -Class AudioEndpoint` shows them as Unknown) are never picked. The playing line logs the endpoint id (`id=wasapi:{0.0.0.00000000}.{guid}`) to tell same-named ones apart.
- **Reading a Windows PC's audio without touching it** (read-only): the log `%LOCALAPPDATA%\dstack\dlisten\<HOST>-dlisten.log` (+ `.1`-`.3`, 10 MiB each since v0.6.228; times UTC); the app status through the local status endpoint (TCP 127.0.0.1, port in the `local status endpoint` start line, send `status\n`, read `.app`); endpoint state changes in `Microsoft-Windows-Audio/Operational` event 65 (`NewState` 1 active, 8 unplugged; read EventData with `[xml]$_.ToXml()`); which process plays on which endpoint: an `Add-Type` C# probe in the MCP Shell over `IMMDeviceEnumerator` + `IAudioSessionManager2` + `IAudioMeterInformation` (sessions with pid and peak per active endpoint; #506 comment issuecomment-7217224184).
- **The log writer** (`logfile::Rotating`): rotation happens at a write that would pass the bound, so a line written in one write stays whole; test with `write_all` of whole lines (a `write!` macro writes in pieces and splits). A failed move appends; a log that could not be reopened is retried at each write.
