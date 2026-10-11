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
   they talk to QEMU's responder; on the laptop the test is the shell's
   `net`, `ping`, `dns` and `http`.
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
