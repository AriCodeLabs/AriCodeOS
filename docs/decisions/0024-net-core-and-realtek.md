# ADR 0024: Shared network core and Realtek r8169 driver

- **Date:** 2026-10-10
- **Status:** Accepted

## Context

The test laptop (Phase 34) has a Realtek `10ec:8136` (r8169 family,
10/100 PCIe) and an Atheros AR9485 Wi-Fi. The whole network stack (ARP,
IPv4, ICMP, UDP, DHCP, DNS, TCP) and the `NetServiceProtocol v1` server
lived inside `services/virtio-net`, tied to the virtio queues, so a second
driver would have duplicated 900 lines. QEMU does not emulate any Realtek
of this family (only the RTL8139, with a different programming model), and
a Wi-Fi driver with WPA2 is months of work: wired networking is the
realistic way for the laptop to use the stack that already exists.

## Decision

1. **`ari-net-core`**: the stack and the server move out of virtio-net
   into a shared user crate, generic over the `Link` trait (`mac`,
   `send_frame`, `recv_frame`) with `run(link, clock, name)`. virtio-net is
   left with its link layer. No protocol or behaviour changes (`network.*`
   are unchanged).
2. **`aricode-rtl8169`** (host library): registers, bits, 16-byte
   descriptors and the family's start-up sequence (`reset`, `start`,
   `ack_interrupts`, `link_state`), over a `Regs` trait that the driver
   implements with MMIO and the tests with memory. It is the only part
   that can be tested without the card, and it is tested there.
3. **`services/rtl-net`** (`rtl_net`): first memory BAR, a DMA buffer with
   16 receive and 8 transmit descriptors (2 KiB per slot), frames padded to
   60 bytes, link awaited for up to 3 s and then `run`. Interrupts via
   MSI/MSI-X (ADR 0022).
4. **init**: without virtio-net, the first Realtek Ethernet function
   (class 02.00) launches `rtl_net` on the same network endpoint. The test
   clients (`net_client`, `tcp_client`, `http_client`) are not run because
   they talk to QEMU's responder, not even with `selftest` (ADR 0032); init
   only reports the address DHCP gave (read-only health check). On the
   laptop the test is the shell's `net`, `ping`, `dns` and `http`.
5. **No QEMU scenario for rtl-net**: there is no emulation. Validation is
   the library on the host, the network scenarios over the shared core and
   the laptop photo (`docs/hardware.md`).

## Consequences

- A third driver (for example e1000e for other machines) only has to
  implement `Link`.
- rtl-net does not program the PHY: it relies on the firmware having done
  autonegotiation. If the link does not come up, it reports it and DHCP
  retries.
- No hardware checksum offload or VLAN; one frame per descriptor.
- Wi-Fi is out of scope: it would require ath9k, 802.11 and WPA2.

## Note (2026-10-18, incident 0012)

A refusal by the service was invisible: the shell printed "network service
rejected the request" for every status it did not know by name, and the
core answered the status without recording anything. Since
`fix/net-refusal-visibility` the core records a fault event per refusal
kind (`net tcp table full`, `net arp defer full`, `net tx busy`, `net arp
no reply`, `net ping table full`, `net dns table full`, `net udp table
full`, `net wait pending`, `net bad request`, `net bad handles`), bounded
to one event per kind per second by `aricode_net::faults::Meter`: the
event name carries the kind and the count in that second (`net tcp table
full x3`), the code the `net::Status` number the client got (5 busy, 1
invalid, 4 bad handles), and refusals held back during a second are
recorded when it ends. `NetError::describe` names every status for the
user. The list and the format are in `docs/architecture.md` (TCP
section); the regression is `network.refusal_visible`. The refusal of the
incident itself (`Busy` or `Reset`, once after a boot on the laptop) is
still open; the audit of the fix opened incidents 0013 (silent SYN drop
and half-open hold) and 0014 (predictable ISN and ephemeral ports).

## Note (2026-10-18, incident 0014)

The core's TCP initial sequence numbers and ephemeral ports were
predictable (`uptime * 64 + slot`, the peer's own number times a constant,
ports from 50000 at every boot). ADR 0034 replaces them: a 32-byte secret
per boot from the kernel generator (`sys::random` with the `SYSTEM` handle
the drivers already hold as their clock, `READ | TRANSFER` from init; no
handle change), RFC 6528 ISNs (`aricode_net::isn::initial_seq`,
HMAC-SHA-256 over the 4-tuple plus a 4 µs clock) for active and passive
opens alike, and RFC 6056 section 3.3.3 ports in 49152..65535. The core
prints `tcp secret from kernel entropy` at start and records the fault
`net tcp secret degraded` if the syscall is refused (the kernel always
answers with the bytes asked and exposes no entropy-quality state, so that
is the only failure the core can see); `network.stack` requires the former
and forbids the latter.

## Note (2026-10-19, incident 0013)

A SYN to a listener with no free TCP slot was the one refusal the core
still dropped silently, and every SYN to a listening port held a slot for
the full data retransmission schedule (about 31 s) with no bound per
listener, so a handful of stray SYNs kept the 8-slot table full for every
active open. ADR 0035 bounds a listener's backlog to `MAX_TCP / 2`
connections nobody accepted (half-open included), answers the SYN beyond
it or without a free slot with a reset (RFC 793 section 3.4) recorded as
`net syn dropped` (code 5), gives the SYN|ACK a budget of 3
retransmissions (about 7.5 s), resets the backlog of a listener that
closes, and adds `Op::TcpStats` so `net` and `diag` show the table by
state. Regression: `network.listen_backlog` and the host tests of
`libs/net`.
