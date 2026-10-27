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
| H1 | **fsd serves a request whose session the same request's sweep has retired** [C-H1]. A sandboxed app fills the 16-entry descriptor table, sends `Open`/`Create` and closes its session handle; the sweep inside `serve` retires the session, and `handle` then runs with no session: whole namespace, no read-only filter. The descriptor stays on the recycled slot for the next session. `services/arifs/src/main.rs:436-529` | **fixed** (incident 0028, merged `02cff0d`) |
| H2 | **A PCI device chooses where the kernel writes** [A-H1]. `table_phys = bar + table_offset` is never compared with the BAR size, BARs are never checked against RAM or the kernel's own MMIO, and the offset is re-read at every claim. The kernel writes the MSI-X entry there; a driver's `Mmio` object maps the reported BAR. The IOMMU does not stop CPU writes. `kernel/src/pci.rs:393-428`, `kernel/src/hw.rs:117-192` | **fixed** (incident 0030, ADR 0051, merged `ab0a99e`) |
| H3 | **Non-critical nameConstraints are ignored** [D-H1]: a sub-CA limited to one domain can vouch for any name. `libs/x509/src/cert.rs:449-452` | **fixed** (incident 0029, merged `24ccb91`) |
| H4 | **Extended key usage is not chained** [D-H2]: an e-mail-only CA can issue TLS server certificates we accept. `libs/x509/src/lib.rs:207,250-263` | **fixed** (incident 0029, merged `24ccb91`) |

### Medium

| ID | Finding | Status |
|---|---|---|
| M1 | **`read_files` has no policy** [E-M1]: a signed manifest can share itself any file of the main namespace, the SSH host key and `random_seed` included; `../` is accepted by the character check. Contradicts ADR 0025. *Reproduced* (manifest parse). | **fixed** (ADR 0050, merged `02cff0d`) |
| M2 | **sshd holds fsd's main endpoint** [E-M2, C-M6]: owner 0, the whole namespace, Bind/Share/Attach, and init's descriptors; descriptors leaked by dead generations accumulate. A pre-auth parser bug would become a persistent backdoor. `services/init/src/main.rs:852` | **fixed** (ADR 0050, merged `02cff0d`) |
| M3 | **GPT `entry_count` is unbounded** [B-M1]: a USB stick with a crafted header makes AriBoot allocate ~512 GiB and abort, so the machine does not boot; init and the updater die likewise. `Partition::sectors` underflows. `libs/gpt/src/lib.rs:99-122` | **fixed** (incident 0032, merged `8763b8f`) |
| M4 | **A partial IOMMU map is never rolled back** [C-M1]: on OOM mid-range, mapped leaves stay in the domain while the frames go back to the allocator. `kernel/src/iommu.rs:400-420` | **fixed** (incident 0041, merged `312cc37`) |
| M5 | **virtio used-ring `id` unchecked** [A-M1, B-L6]: raw-pointer writes up to ~1 MiB past the queue; cyclic chains hang the driver. *Reproduced* on the host. `libs/virtio/src/lib.rs:195-287` | **fixed** (incident 0033, merged `a987f64`) |
| M6 | **AriFS create leaves a ghost entry on an I/O error** [C-M2], which can make the volume unmountable; `write`/`apply_journal` update the size before the table. *Reproduced* on the host. `libs/arifs/src/lib.rs:472,565,669` | **fixed** (incident 0032, merged `8763b8f`) |
| M7 | **USB drivers never close a request's extra handles** [C-M3]: ~85 requests fill the driver's table; keyboard and USB storage stop until reboot. | **fixed** (incident 0033, merged `a987f64`) |
| M8 | **An input driver's waiting place is held by a dead reader, and events go to it** [C-M4]: keys lost after any reader dies. | **fixed** (`6c17a9d`, merged to develop): shared `EventQueue`, events leave only once delivered, reads capped at 10 s; scenario `input.orphan_read` failed before the fix |
| M9 | **Compositor window ids are global and unowned** [C-M5]: an app can poll another's keys or close its window; dead clients' windows are never reclaimed. | **fixed** (incident 0037, ADR 0052, merged `65e33cc`) |
| M10 | **Certificate validity fails open without a clock** [D-M1]: no RTC (or a bad one) skips the time check. | **fixed** (incident 0036, merged `d11c242`) |
| M11 | **Ed25519 signing is not constant time** [D-M2] (SSH host key). | **fixed** (incident 0035, merged `3d4906b`) |
| M12 | **No way to distrust a root or key** [D-M3] (the emSign case). | **fixed** (ADR 0054, merged `d11c242`) |

### Low

| ID | Finding |
|---|---|
| L1 | Device-reported MMIO offsets not checked against the BAR (virtio caps, `notify_off`, xHCI/EHCI capability offsets); xHCI slot id indexes the DCBAA unchecked [A-L1, A-L2] — **fixed** (incident 0033, merged `a987f64`) |
| L2 | The compositor accepts an `Mmio` object as a window buffer, which also moves the device into its IOMMU domain [A-L3] — **fixed** (incident 0037, merged `65e33cc`) |
| L3 | ECAM bus offset underflows for `bus < bus_start` [A-L4] — **fixed** (incident 0030, merged `ab0a99e`) |
| L4 | A remote peer can pin TCP slots for good (Closing never sends; FinWait1 with a zero window has no probe) [B-L3] — **fixed** (incident 0034, merged `2439fe0`) |
| L5 | RSA exponent unbounded: ~11 s of CPU per handshake [B-L4, D-L5] — **fixed** (incident 0029) |
| L6 | WASM block ends rescanned on every entry: fuel no longer bounds run time [B-L5] — **fixed** (incident 0042, ADR 0055, merged `92d393b`) |
| L7 | AriFS layout arithmetic overflows (debug panic, release wrap); `recover` does not bound the journal length [B-L1] — **fixed** (incident 0032, merged `8763b8f`) |
| L8 | DHCP retry counter overflow (debug); predictable DHCP xid and DNS id/port [B-L2, C] — **fixed** (incident 0034, merged `2439fe0`) |
| L9 | Any `net` app can listen on port 22; sshd's failed re-listen is silent [E-L1] — **fixed** (incident 0034, merged `2439fe0`) |
| L10 | Pre-auth SSH user name printed raw (log and terminal injection) [E-L2, C, B] — **fixed** (incident 0035, merged `3d4906b`) |
| L11 | `apps`, `pkgs`, `updater` not reserved as app names [E-L3] — **fixed** (ADR 0050: `apps`, `pkgs`, `sshd`; `updater` stays the updater's own) |
| L12 | `event_record` unmetered: any process can push refusals out of the ring [E-L4] — **fixed** (incident 0040, merged `e2f6192`) |
| L13 | The build host writes `ssh_host_ed25519_key` and stick images mode 0664 [E-L5] — **fixed** (ADR 0050, mode 0600) |
| L14 | Failed spawn leaks frames and ignores the kernel reserve; an orphan child when the parent's table is full; `pci_claim` inserts partially [C-L1..L3] — **fixed** (incident 0041, merged `312cc37`) |
| L15 | init launch error paths leak handles; a failed `start_driver` can leave a device claimed for good [C-L4] — **fixed** (incident 0038, merged `e2f6192`) |
| L16 | The supervisor relaunches a service it retired as stuck [C-L5] — **fixed** (incident 0038, merged `e2f6192`) |
| L17 | console-bridge dropped a reply that arrived during its probe [C-L6] — **fixed** (`a510f21`) |
| L18 | virtio-blk answers a timed-out request before resetting the device [C-L7] — **fixed** (incident 0033, merged `a987f64`) |
| L19 | sshd accepts exec/shell on a channel it already closed [C-L8] — **fixed** (incident 0035, merged `3d4906b`) |
| L20 | The TLS key seed falls back to zeros if `sys::random` fails [C-L9] — **fixed** (incident 0036, merged `d11c242`) |
| L21 | SSH sequence numbers wrap without a forced rekey; strict KEX only when the client offers it (Terrapin with old clients) [C-L10, D-L6, D-L7] — **fixed** (incident 0035, merged `3d4906b`) |
| L22 | A driver's device keeps DMA access to client buffers after `mmio_unmap` [C-L11] — **fixed** (incident 0039, merged `e2f6192`) |
| L23 | X.509: duplicate extensions accepted, a CA accepted as a leaf, CN fallback, wildcards over public suffixes, IP vs dNSName, NUL/control bytes in dNSName [D-L1..L4] — **fixed** except the CN fallback and wildcards over public suffixes (incident 0029) |

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
capability lets any app read the physical keyboard (ADR 0013 point 4;
removed on `fix/compositor-ownership`, ADR 0053).

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

## Progress

- 2026-10-25: M8 and L17 (input, `45e9f00`); H1, M1, M2, L11, L13
  (file service, `02cff0d`; the audit of that branch found that sshd's
  new sessions died with an fsd restart, fixed there with scenario
  `ssh.file_service_restart`); H3, H4, L5, most of L23 (X.509,
  `24ccb91`).
- 2026-10-25: every remaining finding fixed and merged to develop, each
  on its own branch with an incident, a reproduction where one was
  possible, an independent audit and the test level its area needs:
  PCI (`ab0a99e`), kernel error paths (`312cc37`), storage (`8763b8f`),
  drivers (`a987f64`), network (`2439fe0`), SSH (`3d4906b`), TLS and the
  distrust list (`d11c242`), compositor and input (`65e33cc`), supervisor,
  IOMMU and the event ring (`e2f6192`), WASM fuel (`92d393b`). Found on the
  way: incident 0031 (the screen console repaint made test results depend
  on kernel image layout, `fb93332`) and incident 0043 (virtio-blk gave up
  on requests a busy host disk took over 2 s, `2cd0102`). Left as stated:
  the CN fallback and wildcards over public suffixes (L23), and the
  infos below.

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
