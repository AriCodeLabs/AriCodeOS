# Incident 0056: TCP stalled on any loss and trusted blind segments

- Opened: 2026-11-01. Status: fixed on `feature/tcp-recovery` (ADR 0070).
- Found by: the comparison of ADR 0063, step 6. The network service's TCP
  and smoltcp 0.14.0 ran in the same scratch harness:
  - a simulated link with loss, reordering and delay;
  - the Linux kernel's TCP through a TUN device;
  - a scripted peer that injects segments.

  QEMU's user networking loses nothing, so no scenario had ever seen it.

## Evidence

Bulk transfer through the simulated link (100 Mbit/s, drop-tail queue of
100 packets, 4 MiB, median of five seeds):

| Round trip, loss | Before (4 KiB buffers) | Before (32 KiB buffers) |
|---|---|---|
| 1 ms, none | 3.64 MB/s | 11.77 MB/s |
| 1 ms, 0.5 % | 0.003 MB/s | 0.003 MB/s |
| 20 ms, 2 % | 0.003 MB/s | 0.003 MB/s |
| 80 ms, 1 % reordered | 0.003 MB/s | 0.002 MB/s |

Linux sending 16 MiB to the service's TCP through the TUN device, with the
harness dropping packets at random:

| Loss | Before |
|---|---|
| none | 320 MB/s |
| 0.5 % | 0.36 MB/s |
| 2 % | 0.07 MB/s |

The scripted peer, on an established connection, with the stack's
answer:

| Segment | Before |
|---|---|
| RST far outside the window | connection closed |
| RST inside the window, not the next sequence number | connection closed |
| data with an ACK of bytes never sent | delivered |
| data with an ACK a million bytes old | delivered |
| a segment ahead of a hole | dropped |
| the peer's MSS of 536 | segments of 1400 bytes sent |
| zero window, the window update lost | no segment in 30 s, for good |
| RST in SYN-SENT whose ACK is not our SYN's | connection attempt ended |

## Mechanism

1. **Recovery.** After a loss the sender resent one segment per timeout
   and sent nothing else:
   - `snd_nxt` stayed past the lost data, so the window looked full;
   - each ACK reset the timeout to 500 ms;
   - the receiver dropped every segment after the hole, so each timeout
     recovered one MSS.

   That is about 2.8 KB/s, whatever the link, until the window drained.
   There was no fast retransmit, no round-trip measurement and no
   congestion control.
2. **Validation.** Any RST that matched the ports closed the connection,
   and the ACK field of a data segment was never checked (RFC 5961 sections
   3 and 5). A blind attacker who knows the address and port pair needs
   no sequence number to reset a connection, sshd's included.
3. **Window.** A zero window with nothing in flight armed no timer. If the
   peer's window update was lost, the connection waited forever.
4. **MSS.** The peer's MSS option was never read. On a path with a smaller
   MTU, the service's segments were too large.
5. **SYN-SENT.** A reset was believed without checking that it
   acknowledged our SYN (RFC 9293 section 3.10.7.3).

## Fix

ADR 0070 rewrites the connection's transmission and reception:
- the round-trip timeout of RFC 6298;
- go-back-N after a timeout;
- NewReno with fast retransmit;
- SACK both ways, with out-of-order data kept;
- the peer's MSS honoured;
- window probes;
- delayed ACKs;
- RFC 5961's checks.

Buffers grow to 32 KiB for sending and 64 KiB for receiving, from a pool
in netd. After the fix, the same scripted peer finds none of the eight
weaknesses, and the tables of ADR 0070 give the transfers.

## Tests

- `libs/net/src/tcp.rs`, one host test per item of the table above:
  - `only_an_exact_reset_closes`;
  - `data_with_an_impossible_ack_is_dropped`;
  - `out_of_order_data_is_kept_and_sacked`;
  - `the_peers_mss_bounds_every_segment`;
  - `a_closed_window_is_probed_until_it_opens`;
  - `syn_sent_believes_only_a_reset_for_its_syn`.
- `three_duplicates_trigger_a_fast_retransmit`,
  `sack_resends_several_holes_in_one_round` and
  `a_timeout_resends_the_whole_flight` cover the recovery.
- `bulk_transfer_survives_loss_quickly` sends 1 MiB through 1 % and 5 %
  loss. Before the fix it could not finish within its limit.
