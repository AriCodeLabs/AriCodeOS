# Failure-pattern audit: this week's disclosures, applied to AriCodeOS

Date: 2026-10-25. Snapshot: `7342ff1` (develop plus the console bridge
wait-set, branch `perf/input-wait-set`).

## Why

The week's disclosures (Brotli, PHP2Shell, OpenSSH, webpki/JSSE name
constraints, Poly1305, emSign distrust, several Java/Node/PHP memory and
execution bugs) hit languages and programs AriCodeOS does not use. The
owner asked to check the *methods and failures* behind them instead of
the languages: the same mistakes can be made in Rust, in a microkernel,
without any of those programs.

## Method

Six independent, read-only audits, one per failure pattern, over the
whole tree (kernel, services, libs, sdk, apps, boot, xtask):

| Audit | Pattern | Model disclosure |
|---|---|---|
| A | untrusted length, offset or index reaching memory in `unsafe` | Brotli |
| B | panic, abort or unbounded work from attacker-controlled data | parser DoS bugs |
| C | failure path leaving state half-updated; stale state reused across requests, sessions or generations | PHP2Shell, OpenSSH |
| D | crypto, TLS, X.509 and SSH verification gaps | webpki/JSSE, Poly1305, emSign |
| E | a policy enforced on one path but not another; delimiters; traversal | this week's policy bypasses |
| B' | WASM, ELF, update and package parsers (a part of B) | |

Each finding was traced to code by its auditor. Where a reproduction was
possible on the host it was run (marked *reproduced*); the others were
re-read against the code by a second reader. B fuzzed every parser that
takes outside data on the host (about 1.5 billion inputs, overflow checks
and debug assertions on).

## Result

None of the disclosures' exact bugs is in AriCodeOS. Their *patterns*
are, in a few places. The parsers of outside data are strict (no panic
reachable from the network or a USB device in release builds), the
kernel's checks of user pointers and rights are consistent, and the TLS
1.3 and SSH cores are sound. The weak spots are:

- authority that survives the object that granted it (fsd sessions);
- values a device reports, used as addresses (PCI, virtio);
- restrictions on intermediate certificates that are ignored;
- capabilities closed on one path and open on another (`read_files`,
  sshd's fsd endpoint).

## Findings

IDs are this document's; the auditor's own ID is in brackets. Status as
of this document.

### High

| ID | Finding | Status |
|---|---|---|
| H1 | **fsd serves a request whose session the same request's sweep has retired** [C-H1]. A sandboxed app fills the 16-entry descriptor table, sends `Open`/`Create` and closes its session handle; the sweep inside `serve` retires the session, and `handle` then runs with no session: whole namespace, no read-only filter. The descriptor stays on the recycled slot for the next session. `services/arifs/src/main.rs:436-529` | open |
| H2 | **A PCI device chooses where the kernel writes** [A-H1]. `table_phys = bar + table_offset` is never compared with the BAR size, BARs are never checked against RAM or the kernel's own MMIO, and the offset is re-read at every claim. The kernel writes the MSI-X entry there; a driver's `Mmio` object maps the reported BAR. The IOMMU does not stop CPU writes. `kernel/src/pci.rs:393-428`, `kernel/src/hw.rs:117-192` | open |
| H3 | **Non-critical nameConstraints are ignored** [D-H1]: a sub-CA limited to one domain can vouch for any name. `libs/x509/src/cert.rs:449-452` | open |
| H4 | **Extended key usage is not chained** [D-H2]: an e-mail-only CA can issue TLS server certificates we accept. `libs/x509/src/lib.rs:207,250-263` | open |

### Medium

| ID | Finding | Status |
|---|---|---|
| M1 | **`read_files` has no policy** [E-M1]: a signed manifest can share itself any file of the main namespace, the SSH host key and `random_seed` included; `../` is accepted by the character check. Contradicts ADR 0025. *Reproduced* (manifest parse). | open |
| M2 | **sshd holds fsd's main endpoint** [E-M2, C-M6]: owner 0, the whole namespace, Bind/Share/Attach, and init's descriptors; descriptors leaked by dead generations accumulate. A pre-auth parser bug would become a persistent backdoor. `services/init/src/main.rs:852` | open |
| M3 | **GPT `entry_count` is unbounded** [B-M1]: a USB stick with a crafted header makes AriBoot allocate ~512 GiB and abort, so the machine does not boot; init and the updater die likewise. `Partition::sectors` underflows. `libs/gpt/src/lib.rs:99-122` | open |
| M4 | **A partial IOMMU map is never rolled back** [C-M1]: on OOM mid-range, mapped leaves stay in the domain while the frames go back to the allocator. `kernel/src/iommu.rs:400-420` | open |
| M5 | **virtio used-ring `id` unchecked** [A-M1, B-L6]: raw-pointer writes up to ~1 MiB past the queue; cyclic chains hang the driver. *Reproduced* on the host. `libs/virtio/src/lib.rs:195-287` | open |
| M6 | **AriFS create leaves a ghost entry on an I/O error** [C-M2], which can make the volume unmountable; `write`/`apply_journal` update the size before the table. *Reproduced* on the host. `libs/arifs/src/lib.rs:472,565,669` | open |
| M7 | **USB drivers never close a request's extra handles** [C-M3]: ~85 requests fill the driver's table; keyboard and USB storage stop until reboot. | open |
| M8 | **An input driver's waiting place is held by a dead reader, and events go to it** [C-M4]: keys lost after any reader dies. | **fixed** (`6c17a9d`, merged to develop): shared `EventQueue`, events leave only once delivered, reads capped at 10 s; scenario `input.orphan_read` failed before the fix |
| M9 | **Compositor window ids are global and unowned** [C-M5]: an app can poll another's keys or close its window; dead clients' windows are never reclaimed. | open |
| M10 | **Certificate validity fails open without a clock** [D-M1]: no RTC (or a bad one) skips the time check. | open |
| M11 | **Ed25519 signing is not constant time** [D-M2] (SSH host key). | open |
| M12 | **No way to distrust a root or key** [D-M3] (the emSign case). | open |

### Low

| ID | Finding |
|---|---|
| L1 | Device-reported MMIO offsets not checked against the BAR (virtio caps, `notify_off`, xHCI/EHCI capability offsets); xHCI slot id indexes the DCBAA unchecked [A-L1, A-L2] |
| L2 | The compositor accepts an `Mmio` object as a window buffer, which also moves the device into its IOMMU domain [A-L3] |
| L3 | ECAM bus offset underflows for `bus < bus_start` [A-L4] |
| L4 | A remote peer can pin TCP slots for good (Closing never sends; FinWait1 with a zero window has no probe) [B-L3] |
| L5 | RSA exponent unbounded: ~11 s of CPU per handshake [B-L4, D-L5] |
| L6 | WASM block ends rescanned on every entry: fuel no longer bounds run time [B-L5] |
| L7 | AriFS layout arithmetic overflows (debug panic, release wrap); `recover` does not bound the journal length [B-L1] |
| L8 | DHCP retry counter overflow (debug); predictable DHCP xid and DNS id/port [B-L2, C] |
| L9 | Any `net` app can listen on port 22; sshd's failed re-listen is silent [E-L1] |
| L10 | Pre-auth SSH user name printed raw (log and terminal injection) [E-L2, C, B] |
| L11 | `apps`, `pkgs`, `updater` not reserved as app names [E-L3] |
| L12 | `event_record` unmetered: any process can push refusals out of the ring [E-L4] |
| L13 | The build host writes `ssh_host_ed25519_key` and stick images mode 0664 [E-L5] |
| L14 | Failed spawn leaks frames and ignores the kernel reserve; an orphan child when the parent's table is full; `pci_claim` inserts partially [C-L1..L3] |
| L15 | init launch error paths leak handles; a failed `start_driver` can leave a device claimed for good [C-L4] |
| L16 | The supervisor relaunches a service it retired as stuck [C-L5] |
| L17 | console-bridge dropped a reply that arrived during its probe [C-L6] — **fixed** (`a510f21`) |
| L18 | virtio-blk answers a timed-out request before resetting the device [C-L7] |
| L19 | sshd accepts exec/shell on a channel it already closed [C-L8] |
| L20 | The TLS key seed falls back to zeros if `sys::random` fails [C-L9] |
| L21 | SSH sequence numbers wrap without a forced rekey; strict KEX only when the client offers it (Terrapin with old clients) [C-L10, D-L6, D-L7] |
| L22 | A driver's device keeps DMA access to client buffers after `mmio_unmap` [C-L11] |
| L23 | X.509: duplicate extensions accepted, a CA accepted as a leaf, CN fallback, wildcards over public suffixes, IP vs dNSName, NUL/control bytes in dNSName [D-L1..L4] |

### Info

Handle generations are 16 bits with a LIFO free list; DMA quota is not
refunded on a failed allocation; `map_uc` windows are never freed; typed-
ahead console bytes survive a shell crash; the updater treats a boot
state with a bad CRC as fresh and the stable floor fails open when its
section is damaged; secrets are not zeroized; `ct_eq` has no optimization
barrier; AES uses tables; Ed25519 verify accepts small-order keys;
`serve-update` echoes the raw request target into its log and `Location`;
authorized_keys lines with options are ignored silently; `none` and
unsigned publickey queries are not counted as auth failures; the `input`
capability lets any app read the physical keyboard (ADR 0013 point 4).

## Checked and found sound

- Kernel: `check_user_range` and every syscall length cap; IPC handle
  transfer, receive pre-checks and exact-length messages; handle tables;
  rights checks consistent across syscall variants; page, stack, DMA and
  IOMMU table zeroing; DMA lifetime and reap order; PCI claim tokens.
- Parsers of outside data: network stack (no `unsafe`), HTTP and
  redirects, DNS, DHCP, TLS records and handshake, DER, SSH packets and
  authorized_keys, update index, sources, state, history, package
  manifests, kernel command line, initrd, ELF, WASM, USB descriptors,
  HID reports, proto decoders (fuzzed, no panic in release).
- TLS 1.3 handshake and record layer, PKCS#1/PSS/ECDSA strictness,
  Poly1305 and ChaCha20 against an independent reference (3000 vectors),
  DER times, SSH key exchange and MACs, constant-time comparisons where
  secrets are compared.
- Broker launches through one `launch()`, release-key package checks,
  initrd precedence, fsd prefixes and shares, netd ownership, updater
  signatures, downgrade rule and floor, staged file names.

## Fix plan

In this order, each batch on its own branch from develop with an
incident, a reproduction first, an independent audit and the test level
its area needs:

1. fsd authority: H1, M2, M1, L11.
2. Kernel and devices: H2, L3, M4, L14 (full suite, kernel memory).
3. X.509 and TLS: H3, H4, M10, L5, L20, L23, then M12.
4. Storage: M3, M6, L7.
5. Drivers: M5, M7, L1, L18, L22.
6. Network: L4, L9, L8.
7. SSH: L10, L19, L21, M11.
8. Compositor and input policy: M9, L2, the `input` capability.
9. The rest of the lows and infos.
