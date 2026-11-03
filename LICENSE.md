# Licenses

The dstack workspace is MIT OR Apache-2.0 at your choice (`LICENSE-MIT`,
`LICENSE-APACHE`), except one crate:

| Crate | License |
|---|---|
| `dlink` | MIT OR Apache-2.0 |
| `dstack-audio-io` | MIT OR Apache-2.0 |
| `dstack-resample` | MIT OR Apache-2.0 |
| `dstack` (and its binary `dstack-dev`) | GPL-3.0-or-later (`crates/dstack/COPYING`) |

`dstack` keeps GPL-3.0-or-later because some of its files began from
inferno (Teo and the inferno contributors, GPL-3.0-or-later OR
AGPL-3.0-or-later, taken under GPL-3.0-or-later); each one says so in its
header. Once those parts are our own implementation, `dstack` moves to
MIT OR Apache-2.0 too.

Third-party code under `vendor/` keeps its own license (`vendor/mdns-sd`:
Apache-2.0 OR MIT).
