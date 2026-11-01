# Incident 0057: TCP bursts overflowed the NIC's transmit ring

- Opened: 2026-11-01. Status: fixed on `fix/tx-backpressure`.
- Found by: the reference laptop (Realtek 10ec:8136, `rtl_net`) running
  develop 44e91fd as dev build 18. During an SSH output benchmark `diag`
  showed `net tx busy x25` and `x30` per second. The incident was
  introduced by ADR 0070.

## Evidence

On the laptop, `ssh ari@192.168.1.27 "repeat 1000 help"` (1.6 MB) ran at
0.61 to 0.88 MB/s on a LAN with 4 to 50 ms of round trip. The fault ring
showed:

```text
[74.966 s] fault reported by process 6: net tx busy x1 (code 5)
[75.966 s] fault reported by process 6: net tx busy x25 (code 5)
[76.970 s] fault reported by process 6: net tx busy x30 (code 5)
```

The old TCP (develop 9acf9de), measured A-B-A on the same LAN, ran at
0.11 to 0.13 MB/s with no such fault. QEMU's virtio-net never showed it:
QEMU empties the queue when it is notified.

## Mechanism

- `rtl_net` and `virtio-net` had transmit rings of 8 slots.
- The TCP of ADR 0070 sends up to its window, 32 KiB, in one round: 23
  segments, where the old one sent 3.
- netd's `tcp_emit` asked the connection for every segment it had
  (`poll`) and handed each to the driver. `poll` had already counted the
  segment as sent, moving `snd_nxt`. When the ring was full,
  `send_frame` failed and the frame was lost.
- TCP then recovered it as a loss: a fast retransmit, or a timeout, with
  its congestion window halved, on a link that had lost nothing.

## Fix

1. **Backpressure.** `Link::tx_room` says whether the driver has a free
   slot. `tcp_emit` asks the connection for a segment only while it does.
   - A segment that does not fit is never produced: it stays in the
     connection, unsent, with `snd_nxt` unmoved.
   - netd tries again in 1 ms (`tx_retry_at`, in its deadline), unless an
     ACK from the peer wakes it first.
2. **Rings of 32 slots** in both drivers: a full window and the ACKs of
   other connections fit in one round. The DMA grows by 48 KiB per driver.

Other frames (ARP, ICMP, UDP, resets) still go out best effort and are
recorded as `net tx busy` when the ring is full.

## Tests

- QEMU: the `network`, `ssh`, `perf` and `update` areas.
- The laptop, after the fix (dev build 19), the same benchmark and LAN,
  three runs each:

| | Old TCP (9acf9de) | ADR 0070 before the fix | After the fix |
|---|---|---|---|
| `repeat 1000 help` over SSH | 0.11 to 0.13 MB/s | 0.61 to 0.88 MB/s | 1.11 to 1.18 MB/s |
| `net tx busy` in `diag` | none | 25 to 30 a second | none |

  The round trip to the laptop varied between 4 and 42 ms during the runs:
  the key-echo latency follows the LAN, not the stack.
