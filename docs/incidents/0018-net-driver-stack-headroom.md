# Incident 0018: the network driver's user stack has little headroom

- Opened: 2026-10-22. Status: fixed on `fix/net-stack-headroom`
  (2026-10-23); open items at the end.
- Found by: the independent audit of `feature/entropy-source-visible`
  (ADR 0039), after a first version of that branch overflowed the stack
  (below). Predates the branch: the deep path and the frame sizes come
  from `services/net-core` as released in 0.13.0.
- Environment: any. Measured on the release and dev builds of
  `ari-virtio-net` and `ari-rtl-net` (x86_64-unknown-none), 64 KiB user
  stack (65,536 bytes) with a guard page.

## What was seen

While wiring the entropy check into netd, `Stack::new` built the whole
`Stack` (22,312 bytes for `Stack<virtio_net::Nic>`) as a local and
returned it. The compiler inlined `Stack::new` into `run`, whose frame then
held two copies, and virtio-net died in scenario `ssh.entropy_weak` right
after its first pings:

```text
USER FAULT #PF (14) Page Fault: rip=0x40bee0 rsp=0x7f0000 err=0x6 cr2=0x7efff8
USER STACK OVERFLOW: the user stack hit its guard page
[WARN ] user: process 10 'virtio_net' killed: stack overflow
```

The branch then built the `Stack` in place (`Self { .. }` as the tail
expression), and every scenario passed. That was an optimizer outcome, not
a guarantee: nothing in the language or the build kept a future change
from bringing the second copy back.

## Measurement (audit, objdump of the release and dev builds)

- Frame of `run::<virtio_net::Nic>` in the release build: 44,952 bytes
  (the `Stack` lives there, plus the frame buffer and locals).
- Deepest direct-call path, release: `_start` -> `main` -> `run` ->
  `Stack::serve` -> `tcp_room` -> `sweep` -> `probe` -> `send_ipv4` ->
  `send_arp_request` -> `send_frame` = 53,352 bytes of 65,536, about 12 KiB
  left. Indirect calls (trait objects, `core::fmt`) are not counted, so the
  real margin is smaller. rtl-net: 52,200 bytes.
- Dev builds (`cargo xtask build --debug` is supported): the same path is
  69,664 bytes for virtio-net and 68,624 bytes for rtl-net, over 64 KiB.
- There was no stack high-water mark: neither the driver nor the kernel
  could tell how close a process came to its guard page.

## Reproduction (2026-10-23, commit ce29f02)

1. **Static, automated.** `cargo xtask stack-check` (new,
   `tools/xtask/src/stack.rs`, a Rust port of the audit's `frames.py` and
   `depth.py` with host tests) builds both drivers in the release and the
   dev profile, measures every frame from its prologue (return address,
   pushes, `rsp` adjustment, stack probes) and the deepest chain of direct
   calls from `_start`, and fails above half the stack for the chain. On
   the code of 0.13.0 it failed all four binaries:

   | binary | deepest direct path | largest frame |
   |---|---|---|
   | virtio-net, release | 53,368 | 44,952 `run` |
   | rtl-net, release | 52,216 | 44,736 `run` |
   | virtio-net, dev | 69,680 | 38,064 `run` (`serve` 25,232, `on_frame` 25,200) |
   | rtl-net, dev | 68,640 | 37,808 `run` |

   (16 bytes more than the audit's numbers: the return address of
   `_start` is counted.)

2. **At run time, dev build.** `cargo xtask test --debug --all --area
   network` (inside the dev container, `--jobs 6`) on ce29f02: 29 of 30
   scenarios failed, the only pass being `shell.no_network`. It is worse
   than the audit's reading: a dev build of 0.13.0 has no network at all.
   virtio-net dies on the first DHCP offer it receives, 70 times across the
   run (each restart dies the same way until init's budget is spent):

   ```text
   virtio-net: serving requests
   USER FAULT #PF (14) Page Fault: rip=0x4030c9 rsp=0x7efc08 err=0x6 cr2=0x7efc00
   USER STACK OVERFLOW: the user stack hit its guard page
   [WARN ] user: process 16 'virtio_net' killed: stack overflow
   AriInit: service virtio_net died from a fault; restart budget exhausted: system degraded
   ```

   `addr2line` puts `rip` in `Stack::broadcast_dhcp`: `run` (38,064) ->
   `on_frame` (25,200, `on_udp` inlined) -> `broadcast_dhcp` (2,192) is
   66,560 bytes with `_start` and `main`. The table-full path the audit
   named was never reached. Logs: the run's `build/scenarios/*/serial.log`.

3. **The deep path at run time.** Scenario `network.deep_path` and test
   program `tcp_orphan`: the orphan fills the 8-slot TCP table with 7
   connections to the host echo server and one toward 10.0.2.77 (on the
   subnet, no ARP answer from QEMU, so its SYN stays parked), has its ninth
   connect refused after a sweep (which restarts the 1 s sweep period) and
   exits without closing. The shell's `http` at the next prompt finds the
   table full, sweeps inside the request (`serve` -> `tcp_room` -> `sweep`
   -> `probe` -> `retire` -> `reset_and_drop`), and the reset toward
   10.0.2.77 is parked and announced with an ARP request (`send_ipv4` ->
   `send_arp_request` -> `send_eth` -> the NIC). netd names the sweep
   that found the orphan gone (since 95fee49): `reclaimed (sweep for a full
   tcp table)` is the one inside the request, and the scenario requires it
   and forbids `reclaimed (periodic sweep)`; `net arp defer full` absent
   shows the reset got a parking slot. The scenario runs alone (`serial`):
   the request must reach netd within the 1 s before the next periodic
   sweep, in wall-clock time. On the dev build
   of ce29f02 it failed with the overflow above, before reaching the deep
   path; it was not run on the release build of the old code (the static
   margin there was about 12 KiB, indirect calls not counted).

## Mechanism

Every byte of the network stack's state and of its scratch buffers lived
on the driver's 64 KiB stack:

- `run` held the `Stack` (22,312 bytes: 8 UDP sockets with a 1,472-byte
  buffer each, 4 ARP parking frames of 1,514 bytes, the session tables)
  plus the receive buffer, and whether `Stack::new` added a second copy
  was the inliner's choice.
- Every new TCP socket was built as `Box::new(TcpSock::new(conn))` inside
  `on_tcp`, `TcpConnect` and `TcpListen`: the 8 KiB `Connection` (two
  4 KiB rings) and the `TcpSock` around it were temporaries in the frames
  of `on_frame` and `serve`, several times over in a dev build (opt-level
  1 keeps one stack slot per temporary): 25 KiB each.
- The two big frames sit on top of each other on every path that receives
  a frame or serves a request, so the remaining few KiB were all the
  deeper calls had.

## Fix (commit 65828be)

- `Stack::new` returns the stack in a `Box`, is `#[inline(never)]`, and a
  const assertion (`STACK_STRUCT_MAX`, 8 KiB) makes a larger `Stack` a
  build error, so even the copy an unoptimized build makes on the way into
  the `Box` stays small and is popped before the service loop.
- Heap instead of stack: the UDP socket buffers (`heap_buffer`, allocated
  as a zeroed `Vec`, never built on the stack), the four ARP parking frames
  (allocated once in `Stack::new` and reused: parking copies the frame in,
  no allocation on the send path) and the receive buffer of `run`.
- `TcpSock::boxed`, out of line, builds every TCP socket: the
  `Connection` temporary of a dev build is in that one leaf frame (8,320
  bytes), not in `serve` or `on_frame`.
- `Stack` is now 4,680 bytes over virtio-net. Heap use grows by what
  left the stack: the `Stack` itself, the four parking frames (6,056
  bytes) and the receive buffer (1,514) from the start, plus 1,472 bytes
  per bound UDP socket: about 24 KiB with all 8 UDP sockets bound, on top
  of the 8 KiB per TCP socket that were already on the heap, in the
  256 KiB arena.

After the fix (`cargo xtask stack-check`):

| binary | deepest direct path | was | largest frame |
|---|---|---|---|
| virtio-net, release | 11,752 | 53,368 | 3,456 `run` |
| rtl-net, release | 10,872 | 52,216 | 3,456 `run` |
| virtio-net, dev | 15,408 | 69,680 | 8,320 `TcpSock::boxed` |
| rtl-net, dev | 14,736 | 68,640 | 8,320 `TcpSock::boxed` |

The release deepest path is now the audit's path itself (`serve` 2,128 ->
`tcp_room` -> `sweep` -> `probe` 1,648 -> `send_ipv4` 1,616 ->
`send_arp_request` 1,568): 18% of the stack, 23% in a dev build.

## Guards

- **Build (commit 0341eb1).** `cargo xtask check` ends with
  `stack-check`: it fails if, in any of the four binaries, the deepest
  direct path passes 32 KiB (half the stack) or one frame passes 16 KiB (a
  quarter). The frame budget was first 8 KiB (commit ce29f02); it became a
  quarter of the stack when the fix showed that the one frame between 8 and
  16 KiB, `TcpSock::boxed` in the dev build, is the socket itself, not a
  buffer. A unit test fails if the kernel's `USER_STACK_SIZE` changes
  without the budgets. Budgets and limits: `tests/README.md`.
- **Run time (commit a048158).** `system_info(STACKS)` gives each live
  process's stack high-water mark: the kernel finds the lowest page of the
  user stack whose Accessed bit the CPU set. `spawn` zeroes the stack pages
  through the kernel's own mapping, and nothing clears the bit, so only the
  process's accesses (and the kernel's copies to and from its buffers)
  count, page granular; the stack probes of large frames make a deep call
  visible even if it wrote little. Unlike the static check it includes
  indirect calls. `diag` lists it (`user stacks (touched of size, page
  granular)`), and `network.deep_path` requires virtio-net's line.
- Also found while reproducing (commit c2395ba): `cargo xtask test
  --debug` could not run at all, because the dev build of the WebAssembly
  demo (636,854 bytes against 12,050) does not fit the test disk's ARI0
  and `write_test_disk` stopped with `OutOfRange`. The demo is now always
  built in release.

## Tests (2026-10-23, inside the dev container, `--cpus 10 --memory 12g`)

- `cargo xtask check` (fmt, clippy, stack-check) and `cargo xtask test
  --level unit` (517 host tests, among them the parser and budget tests of
  `stack.rs` and `stack_entry_round_trip`) green after every commit.
- Dev build, `cargo xtask test --debug --all --area network --jobs 6` on
  c2395ba: no `USER STACK OVERFLOW` in any of the 30 scenarios (70 before);
  17 pass, among them every `network.*` (`network.deep_path` included),
  `ssh.*`, `net.https`, `crypto.entropy_rdrand` and `shell.commands`. The
  13 that fail are the update scenarios (`update.*`, `apps.downgrade_*`,
  `apps.upgrade_network`), for a reason unrelated to the stack: a dev
  system image does not fit the updater's 4 MiB image buffer (incident
  0021). In `network.deep_path` the reclaim came after the orphan's exit
  and before `HTTP 200`, the parked reset later expires as
  `net arp no reply`, and `diag` shows virtio-net at 16 of 64 KiB touched,
  against 15,408 bytes measured statically.
- Release build, the full suite (`cargo xtask test --all --jobs 10`, run
  once because the branch touches the kernel's page-table reading): 166
  scenarios, 0 failed, 4m51s. `diag` in `network.deep_path`: virtio-net
  12 of 64 KiB (static 11,752 bytes), fsd 16, shell 24.
- Host temperature during the runs: 85 °C at most, the thermal guard never
  paused a guest.

## Audit follow-ups (2026-10-23)

The independent audit of b1cea10 found nothing critical and re-measured
the numbers above exactly. Its follow-ups, each a commit:

- **M1, every initrd program under budget (ec6305d, 5548917).** The check
  covered only the two network drivers, but in release builds the shell
  (39,448-byte path, 19,424-byte frame in `http_get`: every HTTPS download
  runs the certificate checks, `x509::verify_chain` -> `rsa::verify_pss` ->
  `bigint::Modulus::pow`, below that frame, and the shell is every SSH
  session), usb-input (23,240 / 20,000, `main`), usb-ehci (20,104 /
  17,296, `main`), fsd (20,392 / 14,288) and the updater (28,408 / 10,576)
  were near or over it. Fixed rather than excepted: the shell's 8 KiB body
  buffer and the download's 8 KiB header buffer are on the heap
  (`aricode_http::fetch_with_head`, the header buffer given by
  `aricode_https`), and both USB drivers' `open` return the controller in a
  `Box`. Now, release / dev: shell 23,112 / 21,104, updater 20,248 /
  18,960, usb-input 4,168 / 6,464, usb-ehci 4,664 / 6,640, fsd 20,392 /
  25,216. `stack-check` measures all 63 initrd programs (126 binaries),
  with per-binary budgets (32 KiB / 16 KiB by default; exceptions only in
  `stack::EXCEPTIONS` with a reason and a tracking incident, none today).
- **L3 (5548917).** Functions are keyed by address (three demangled names
  repeat in a dev virtio-net, and the last one used to overwrite the
  others), a branch target resolves to the function containing it,
  conditional tail calls are followed, and the module and
  `tests/README.md` list what the lower bound leaves out.
- **M2 (ed95188).** The high-water scan is
  `aricode_x86_paging::stack_touched`, a pure function with host tests
  (lowest accessed page, Accessed bit only, range); scenario
  `diag.stack_high_water` runs `stack_probe`, which nests two 12 KiB frames
  and requires its own entry to be at least 24 KiB and less than the whole
  stack.
- **L1 (95fee49).** The reclaim line names its sweep, and
  `network.deep_path` asserts the one inside the request (above).
- **L7 (3d7e1cf).** A fixed byte vector pins the `StackEntry` layout and
  its zero padding.
- **L2, L5, L8 (this documentation).** The updater buffer is incident
  0021; ADR 0039 and `docs/architecture.md` record what `STACKS` exposes;
  the heap estimate above is corrected.

Tests of the follow-ups (inside the dev container, same limits):
`cargo xtask check` (126 binaries in budget) and the unit level (523 host
tests, with the new ones of `stack.rs`, `stack_touched` and
`stack_entry_wire_layout`) after every commit; `cargo xtask test --all
--area network,ssh,diag,shell,usb,tls --jobs 10` on 3d7e1cf: 87 scenarios,
0 failed, 3m39s. `stack_probe: touched 28672 of 65536 bytes after two 12
KiB frames: ok`; `network.deep_path` and `network.sockets_reclaimed` both
reclaimed by the `sweep for a full tcp table`; virtio-net 12 of 64 KiB.
Host temperature at most 85 °C, no guest paused. The full suite was not
rerun: the kernel change since the full run is the move of the scan into
`aricode_x86_paging` (same logic, now tested on the host), and the dev
network run was not repeated (the netd change since is the reclaim line).

## Open

- The frame budget is 16 KiB because a dev build copies the 8 KiB TCP
  socket through `TcpSock::boxed`'s frame; building it in place (it would
  need an in-place constructor for `aricode_net::tcp::Connection`) would
  let the budget go back to 8 KiB. Not done.
- `fsd` is the program closest to its budget in a dev build (25,216 of
  32,768 bytes, `main`'s frame 10,176); it passes.
- The static analysis does not follow indirect calls; the high-water mark
  in `diag` is the run-time measurement that covers them, but nothing
  fails automatically on it yet.
- `STACKS` gives any holder of `READ` on `SYSTEM` every process's stack
  depth, page granular: a weak side channel, recorded in ADR 0039 and
  `docs/architecture.md` and kept.
