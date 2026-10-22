# Incident 0018: the network driver's user stack has little headroom

- Opened: 2026-10-22. Status: open, measured; no fix yet (planned on its
  own branch).
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

The branch now builds the `Stack` in place (`Self { .. }` as the tail
expression), and every scenario passes. That is an optimizer outcome, not
a guarantee: nothing in the language or the build keeps a future change
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
  A dev build's netd dies with `USER STACK OVERFLOW` when the TCP table is
  full and the sweep resets a dead client's connection toward an address
  with no ARP entry (the path above). Not reproduced in a scenario yet;
  the measurement is static.
- There is no stack high-water mark: neither the driver nor the kernel can
  tell how close a process came to its guard page.

## Plan

1. Move `Stack` off the stack: a `static` (one per driver process) or a
   `Box` allocated in `run`, so the frame of `run` drops by about 22 KiB
   and no inlining decision can double it.
2. An xtask check of frame sizes and of the deepest direct-call path per
   service binary, from `objdump` of the built ELF, failing above a budget
   (the audit's `frames.py` and `depth.py` are the starting point; they go
   into `tools/` with the fix, not before).
3. A kernel stack high-water mark per process from the Accessed bits of
   the user stack's page table entries, shown in `diag` / `ps`, so the
   margin is measured at run time too.
4. A scenario that drives the deep path (full TCP table, dead client,
   destination without an ARP entry) under a dev build.

## Open

- Everything above; the 0.13.0 release builds are not known to overflow,
  but their margin (about 12 KiB, indirect calls not counted) is not a
  bound.
