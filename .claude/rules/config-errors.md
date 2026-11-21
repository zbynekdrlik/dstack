---
paths:
  - "crates/dstack/src/device.rs"
  - "crates/dstack/src/device/tests.rs"
  - "config/*.toml"
---

# Config and state parse errors

- **What may reach a log** (#313): `device::redact` cuts every quoted span; a backticked word shows only when it is exactly in `CONFIG_KEYS` (DeviceConfig's keys, `[[subscribe]]`'s, the `timing` names). A new config key, nested `deny_unknown_fields` table or enum adds its names there; `the_config_keys_are_the_schema_s_serde_keys` reads serde's own "expected" lists and fails until it does. The state file's errors keep no key.
- **serde's quoting is not symmetric** (toml 0.9): values are Debug-quoted (`"..."`, backslash escapes), but a typed key or enum value is backticked RAW, so a key holding a backtick splits naive pairing and leaks its tail. Typed integers and floats are backticked too (`invalid value: integer `99``). So the message's first backticked span runs to the last `` `, expected `` (or the last backtick), never to the next backtick. A struct-variant enum would make toml list unexpected keys raw and unquoted (`unexpected keys in table: ...`), which no quote rule can cut: keep it out of the config, or redact that message shape first.
- **Probe a message shape** with a throwaway unit test printing `toml::from_str::<T>(..).unwrap_err().message()`, never from memory; remove it before the commit.
- **A removed config key** (#444, `mdns_bind_address`, 21.11.2026): `deny_unknown_fields` refuses a config that still names it, and since the key left `CONFIG_KEYS` its name is redacted in the message (`unknown field `…``): a test of the removal asserts `unknown field` and that the old name is NOT in the message, never that it is. Name the removal and the line to delete in the CHANGELOG; grep the consumers for a `DeviceConfig` literal naming the field (iemmixer's tests build one).
