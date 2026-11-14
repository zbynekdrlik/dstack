# A Dante peer for an application's CI (dstack #305)

An application that embeds a dstack device (dlink: obs's dante-input, the
intercom, any app) can gate its release on a real Dante exchange in CI, with
no Dante network and no grandmaster: `dstack-dev peer` is a second Dante
device on the runner. It sends a tone the app receives, receives the app's
TX, and writes a JSON status the job gates on. Both devices run on the TEST
system clock, skewed by the same `test_clock_ppm`, so the app's play
times, its ASRC and the TX Player see real drift (the field has about 6 ppm).

`dstack-dev` is in every release's assets:
`https://github.com/zbynekdrlik/dstack/releases/download/<tag>/dstack-dev-<tag>-linux-x86_64`
(with its `.sha256`).

## The runner's network: a dummy interface

GitHub's `ubuntu-24.04` runners have passwordless sudo.

```bash
sudo ip link add dante0 type dummy
sudo ip addr add 10.70.0.1/24 dev dante0
sudo ip link set dante0 up multicast on
# Dante's multicast (mDNS, the info groups, multicast flows) on dante0:
sudo ip route add 224.0.0.0/4 dev dante0
```

Both devices use the one address. Linux loops a host's own multicast back to
its members, so they discover each other over mDNS and exchange flows as two
devices on one network would. The app's device keeps Dante's default ports;
the peer takes its own ports and process id (two devices on one address).

## The app's device

On the TEST system clock with a skew, through dlink:

```rust
let config = dlink::Config::new("obs-ci", state_path)
    .rx(["In 1"])
    .tx(["Out 1"])
    .subscribe(1, "TX 1", "ci-peer") // the peer's tone
    .test_clock_ppm(6.0); // TEST ONLY: selects the system clock
let link = dlink::Link::start(&config, &dlink::Place::Iface("dante0".into()), map, processor)?;
```

`Config::test_clock_ppm` selects `Clock::System`; `Link::start_on` with any
other clock refuses it. A device config file takes the same key
(`test_clock_ppm = 6.0`, with `--clock system`). The device logs a WARN at
start naming the skew. -1000 to 1000 ppm; keep it within ±50 ppm: the
play-time filter follows a rate gap up to about 100 ppm.

## The peer

```bash
./dstack-dev peer --iface dante0 --name ci-peer \
  --tone-hz 1000 --rx-from obs-ci:Out\ 1 \
  --test-clock-ppm 6 \
  --ports 4441,8801,4456 --process-id 1 \
  --status "$RUNNER_TEMP/peer.json" &
PEER=$!
# ... run the app's test (it receives "TX 1" of ci-peer, sends "Out 1") ...
kill -INT "$PEER"; wait "$PEER"   # exit 0 after the goodbyes
```

- `--tone-hz <hz,...>`: one TX channel ("TX 1", "TX 2", ...) per tone, a
  sine at -6 dBFS on the media sample grid. Whole Hz, 200 Hz or over (the RX
  check's margin), under half the rate.
- `--rx-from <device>:<channel,...>`: one RX channel per name, subscribed to
  that TX channel of the app's device.
- `--test-clock-ppm <ppm>`: the peer's skew. Give it the app's skew: both
  devices then keep one network time (as PTP gives in the field), and both
  run that many ppm off the machine's clock, the drift the app's play times,
  ASRC and TX Player meet. Each skewed clock counts from its own start, so
  two started `t` apart differ by a constant `t × ppm` (a minute at 6 ppm:
  0.36 ms, as a path delay). Different skews make the devices drift apart:
  the receiver's margin (about 8 ms of a 10 ms latency) is gone after
  margin / Δppm (11 minutes at 12 ppm apart), so only a short run takes them
  (dstack's own two-device test runs 10 s at 70 ppm apart).
- `--rate` (48000), `--latency-ms` (10), `--state` (`<status>.state.toml`).
- `dstack-dev peer --help` lists them. Exit 2 for the command line or the
  config, 1 when the device fails or the status cannot be written.

## The status (rewritten once a second)

```json
{"peer": {"name": "ci-peer", "version": "0.6.137", "test_clock_ppm": -6.0,
          "tx": [{"channel": "TX 1", "tone_hz": 1000}],
          "rx": [{"channel": "RX 1", "from": "obs-ci:Out 1", "locked": true,
                  "tone_hz": 1000.0, "level_dbfs": -6.02,
                  "samples": 2880000, "tone_gaps": 0}],
          "blocks": 22500, "play_at_ppm": -6.0},
 "device": { ... the device's own status, as `dstack-dev ctl status` ... }}
```

- `rx[].tone_gaps`: discontinuities of the received tone, counted once the
  tone locked (a skipped, repeated or missing sample, a dropout; one count
  per run of bad samples). The check needs no known frequency: it fits each
  block to a pure sine and judges the next block by it, so the app may send
  any steady tone (`tone_hz` and `level_dbfs` show what it heard).
- `play_at_ppm`: the peer's media clock against its app clock as its blocks'
  play times show it: its own `test_clock_ppm`, a check the skew is live.
- `device.engine.play_at_steps` / `play_at_max_dev_us` (dstack #305, every
  dlink device's status has them): blocks whose `Block::play_at` moved more
  than 1 ms off their media time since the block before, and the largest
  such deviation since the start, µs.
- `device.channels[]` (each RX channel's ring): `holes`, `late`
  (`start_holes` are the ring's start, before the first packet: not a
  fault); `device.rx[]` (each receiving flow): `gaps`, `late`,
  `socket_drops`; `device.engine.rebootstraps`.

## What to gate on

`tools/linux/tests/ci-peer.test.sh` is this recipe with two peers (dstack's
own CI job `ci-peer`): a working example of the setup and the gate below.

Read both statuses after a settle (say 5 s after both RX channels lock) and
again at the end; gate on the rise between the two, as dstack's own soak
(dstack #304) gates after priming:

The peer (`peer.json`):

- `peer.rx[].locked` true, `tone_gaps` rise 0;
- `device.channels[]` `holes` and `late`, `device.rx[]` `gaps`, `late` and
  `socket_drops` rise 0;
- `device.engine.rebootstraps` rise 0, `play_at_steps` 0;
- `peer.play_at_ppm` within ±2 of the peer's `test_clock_ppm`.

The app (its dlink status, `Link::status()`, and its own bridge counters):

- TX (the app's dlink `bridge::Stats` of its TX player: `underruns()`,
  `recentres()`, `resyncs()`, `ppm()`, which the app publishes in its own
  status, obs as `tx_underruns` and its kin): underruns and
  re-centres rise 0 after priming; the correction settled within ±(the skew
  + 2) ppm;
- RX: the bridge's underruns rise 0, `channels[]` `holes` / `late` rise 0;
- `engine.play_at_steps` 0 and `play_at_max_dev_us` under 1000.
