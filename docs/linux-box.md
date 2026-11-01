# A Linux box for low latency

What a Linux box needs before a dstack device on it meets a receiver's
latency of 250 µs to 1 ms. Each item was found on the gen3 box (N100,
Realtek r8169) with the IEM card's latency records as the judge
(dstack #1, 31.10.2026). Our own counters were clean in every case:
only a real receiver showed the fault.

| Need | Why | Check |
|---|---|---|
| Real-time priority for the service (`LimitRTPRIO=49` or a limits file) | TX runs at `SCHED_FIFO` 47, RX at 44, the engine pump at 40, all below the IRQ threads and dantesync at 50. At normal priority, any other task delays a send. | Start line: `TX thread runs SCHED_FIFO priority=47`. |
| No deep CPU idle states while TX runs | With C6/C8/C10 on, the package sleeps between packets and every send waits for it to wake: card peak 490 µs and late packets. The TX thread holds `/dev/cpu_dma_latency` at 0 (0.4.9), but the device is root's by default. | Grant the device to the service's group (udev: `KERNEL=="cpu_dma_latency", GROUP="<group>", MODE="0660"`), or disable the idle states. Start line: `CPU wake latency held while TX runs`. |
| `noqueue` on the Dante NIC | fq_codel, the kernel's default, holds our packets behind a preempted low-priority sender: card peak 812 µs and late packets with our side clean; `noqueue` gave peak 229 µs and none late. | `tc qdisc replace dev <nic> root noqueue`, made permanent. Start line: `the Dante NIC sends without a queue` (0.4.13 warns otherwise). Watch `send_errors` in the status: with no queue, a full NIC ring drops. |
| TSO and GSO off on the Dante NIC | A NIC with one TX ring (r8169) sends a 64 kB TSO segment from another program on the box in about 525 µs, and our packet waits behind it: with `noqueue`, an hour gave card peak 750 µs and 47 late packets. With TSO/GSO off, peaks of 198 to 260 µs and 1 late packet in an hour. | `ethtool -K <nic> tso off gso off`, made permanent (a udev rule or the network manager's link config). `ethtool -k <nic>` shows `tcp-segmentation-offload: off`. |
| `CAP_NET_RAW` for the service (0.4.15) | With one TX queue, every sender on the box shares the qdisc. On PREEMPT_RT, a preempted low-priority sender that holds it keeps our packet waiting: a TCP burst or an ssh login gave card peaks of 250 to 1800 µs. With `CAP_NET_RAW`, unicast audio goes past the qdisc to the driver. | `AmbientCapabilities=CAP_NET_RAW` in the service unit. Start line: `audio TX bypasses the qdisc`. The status's `direct` count grows with `packets`. Keep shaping the non-audio traffic (prio + tbf). A second NIC for that traffic is better still. |
| RX apart from TX | A received flood on the TX core delayed sends by up to 247 µs. TX preempts RX (0.4.10), RX reads in passes (0.4.11). | Optionally `rx_cpu` (0.4.12) on the core the NIC's interrupts land on. |
| `tx_margin_ns` small | The margin adds 1:1 to every packet's lateness at the receiver. | 0 to 30 µs at 250 µs. |

The gate: read the receiver's own latency records (the data behind Dante
Controller's Latency tab) for the flows from the device, for hours: no
late packet, and every peak under the flow's latency.
