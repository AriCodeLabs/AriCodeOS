# Incident 0034: TCP slots a peer could pin, a port anyone could take, guessable DHCP and DNS ids

- Opened: 2026-10-25. Status: fixed on `fix/network-hardening`.
- Found by: the failure-pattern audit of 2026-10-25 (audit B: L2, L3;
  audit E: L1; audit C: DNS note). Not seen on a machine.
- Environment: netd (`ari-net-core`, `aricode-net`) and sshd.

## Evidence

- TCP (B-L3, reproduced by the audit on the host): a peer that opens with
  a 10-byte window, lets our owner send and close, then sends its FIN
  while 90 bytes and our FIN are pending left the connection in CLOSING:
  `poll` never sent in CLOSING and no timer was armed. A peer keeping a
  zero window did the same in FIN-WAIT-1. netd frees only closed
  sockets, so eight such peers held every TCP slot, sshd's listener
  included, until the next boot.
- Ports (E-L1): any program with `net` could listen on any port, 22
  included, while sshd was down or restarting; with a stolen host key
  (fixed by ADR 0050) it could pose as sshd. sshd's failed re-listen was
  silent.
- DHCP and DNS (B-L2, C): the DHCP transaction id was
  `0x41524900 | mac[5]`, the DNS ids counted from 1 on a fixed source port,
  and DNS answers were accepted from any address and port; the DHCP retry
  counter overflowed after 256 tries in a debug build.

## Fix

- `aricode_net::tcp`: CLOSING sends pending data and FIN like the other
  closing states; FIN-WAIT-1, CLOSING and LAST-ACK give up after
  `CLOSING_MS` (30 s) with an RST, as FIN-WAIT-2 already did. Host tests
  `closing_sends_what_is_pending`, `a_closed_connection_gives_up_after_closing_ms`.
- Ports below `PRIVILEGED_PORTS` (1024) need the session to have been
  given that port by `Attach` (`Request::port`); init gives sshd's
  session `SSH_PORT`, no other. Refusals answer `Denied` and record `net
  port reserved`. sshd says once when it cannot listen on 22 again.
- netd draws the DHCP xid, the first DNS id and the DNS source port at
  random (never from the TCP secret), takes DNS answers only from the
  configured server's port 53, and saturates the DHCP retry counter.

## Audit of the fix

- H1: the first DNS source check accepted answers only from the
  configured server, which broke lookups that name their own server (the
  boot test `net_client` resolves through the test responder). Each
  pending query now records where it went; an answer counts only for the
  query with its id that went to the address and port it comes from.
- L1: every DNS query gets its own random id (none a pending query has).
- L2: an answer must carry one question, not be truncated; a nonzero
  rcode is no address.
- L3: a short random read for the starting ids is said.
- L5, L6, L7: stronger and new TCP tests (the FIN's sequence from
  CLOSING, LAST-ACK's deadline); `Denied` in the status tests with the
  "network service refused" wording; docs.
- Accepted residual risk (M1): an active attacker can still keep closed
  connections in FIN-WAIT-1 with a zero window, each for at most
  `CLOSING_MS`, and so keep TCP slots busy while the attack lasts. A full
  table recycles only TIME-WAIT slots; recycling the closing slot that
  ends soonest (with an RST) would bound it further.
