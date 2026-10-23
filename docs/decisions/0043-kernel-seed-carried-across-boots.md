# ADR 0043: Kernel seed carried across boots

- **Date:** 2026-10-23
- **Status:** Accepted (closes the open item of incident 0014, ADR 0034 §4
  and ADR 0039 "Consequences"; amends ADR 0039 §2 to §5 and ADR 0025
  "Entropy")

## Context

The reference laptop has no usable hardware entropy: its `diag` reads
`entropy  TSC + interrupts (72 samples, 1737 events): weak`
(`docs/hardware.md`, "Entropy"). Its kernel generator is seeded from cycle
counter samples and the interrupt-timing pool only, so netd's TCP secret
(ADR 0034) and any key the machine generates rest on what one boot
gathered by the time it is drawn. sshd already carries its own
`random_seed` across boots (ADR 0025), but only for itself and only once
the file service is up; netd has no file service. ADR 0039 made the
weakness visible and named the deeper fix: a seed kept by the kernel
generator from one boot to the next.

The kernel cannot read files. A seed reaches it either from the boot
loader, which runs before it with the firmware's disk access, or from user
space after the file or block service is up.

## Decision

1. **The boot loader carries the seed, not init.** AriBoot reads the seed,
   writes its successor and hands the kernel its share in `BootInfo`;
   init only replaces the stored seed later with output of the kernel
   generator. The init path (init reads a seed file and passes it to the
   kernel through a new privileged "mix" syscall) was rejected:
   - **It arrives too late on the machine this is for.** netd draws its
     TCP secret in `Stack::new`, as its driver starts. On the laptop the
     network driver (`rtl_net`, init step 8) starts before the USB storage
     stack (step 9b), so a seed coming from the stick's file system would
     reach the kernel after netd had drawn its secret. Through the loader,
     the seed is in the generator before the first process runs.
   - **No new kernel surface.** A mix-only syscall gated by a capability
     only init holds would be new privileged ABI. The loader path needs
     none: `BootInfo` grows by an appended field (its `size` says whether
     it is there), and init writes back with the `RANDOM` read and the
     block service it already uses.
   - **Both write paths already exist and run on the laptop.** AriBoot
     writes sector 0 of `ARIBOOT` (the boot state, its boot counter) on
     every boot through UEFI `BlockIO`, on the laptop's stick since 0.9.0;
     init writes the same partition to confirm a slot.
2. **The store is two sectors, not a file.** Sectors 1 and 2 of the
   `ARIBOOT` partition (64 sectors; sector 0 is the boot state) hold two
   copies of a record (`aricode_ab::seed`): magic `ARISEED\0`, version 1,
   a generation that grows by one per write, the 32-byte seed and the
   first 8 bytes of SHA-256 over the rest; everything else zero. An ESP
   file would need FAT writes through the firmware's file system protocol
   in the loader (never exercised on the laptop) and a FAT writer in user
   space; an `ARI0` file would need AriFS in the loader. Raw sectors need
   neither. The store is not a file at all, which meets "not readable by
   apps" more strictly than a private fsd file: no file grant, `cat`,
   `diag NAME` or `cargo xtask fs-cat` reaches it. Only holders of the raw
   block service read it: init, the updater (`caps = block`) and test
   programs, which can already rewrite the system slots.
3. **A seed is never used twice.** Every write goes to the copy not in
   use with the next generation and only then is the other copy blanked;
   readers take the intact copy with the highest generation
   (`seed::pick`, `seed::plan`).
   - AriBoot reads both copies; with a seed it computes the kernel's share
     and the successor, both HMAC-SHA-256 keyed with the seed over a label
     and eight cycle-counter samples taken around the reads
     (`seed::split`), writes the successor (write and flush), blanks the
     old copy (write and flush), and only then puts the share in
     `BootInfo`, so a seed a kernel used never stays on the disk. A power cut
     before the successor is on the disk leaves the old seed, which no
     kernel has used; after it, the next boot finds the successor. If the
     successor cannot be written, or the old copy cannot be blanked, the
     share is not handed over at all (status `NotSaved`): a later boot
     could find the same seed (the next one directly, or after a damaged
     successor). The
     successor on the disk reveals neither the seed nor the kernel's
     share.
   - The loader writes no first seed: it has nothing better than the
     cycle counter. A blank or unreadable store is left to init.
   - init replaces the store with 32 bytes of `RANDOM` (generator output,
     never its state) as soon as the system disk answers ("early", which
     also writes the first seed of a new disk) and again after `boot
     services completed` ("after the boot services"), when the network,
     USB and disk interrupts of the whole boot are in the generator. Each
     write is flushed before the other copy is blanked, so a torn write or
     a reordering device leaves at least one intact copy.
   - Images ship no seed: `cargo xtask usb-image` and the test disks zero
     both copies (`init_ariboot`), so no two machines flashed from one
     image share a seed. Every machine's first boot is weak and says so.
     Copying an installed disk copies its seed; that is the owner's act.
   - The seed lives in memory as briefly as it can: the kernel copies its
     share out of `BootInfo` at entry, before anything else, and zeroes it
     there (the `BootInfo` page is kept for the whole boot); the seed
     hashes it in once and zeroes the copy. AriBoot and init wipe their
     buffers, and init's block buffer, with volatile writes
     (`aricode_crypto::wipe`). Frames the loader leaves behind are zeroed
     by the kernel before any process sees them.
4. **The kernel reports it.** `BootInfo` gains `carried_seed` (32 bytes)
   and `carried_seed_status`, appended after `phys_map_limit`; a kernel
   reads them only when `size` covers them, so an older loader means
   `NoStore` and an older kernel ignores them (`BOOT_PROTOCOL_VERSION`
   stays 4). The seed hashes the share in after the `rdrand` values and
   TSC samples. ADR 0039's `RandomSource` grows by the `carried` byte
   (byte 32, `CarriedSeed`: `NoStore` 0, `Mixed` 1, `Missing` 2,
   `Unreadable` 3, `NotSaved` 4; bytes 33..40 zero), so `RANDOM_SOURCE`
   answers 40 bytes; `decode` still accepts an older kernel's 32 bytes as
   `NoStore`, and `sys::random_source` passes an 80-byte buffer. The
   kernel's boot lines are `random: seed carried from the previous boot
   mixed in (ADR 0043)` or `random: no carried seed: ...` with the reason.
5. **A carried seed counts as strong.** `RandomSource::is_strong` is a
   hardware source or `carried == Mixed`; netd's TCP secret is then
   `tcp secret from kernel entropy (TSC + interrupts + carried seed)`
   with no `net tcp secret weak` event, sshd draws a host key without
   `ssh entropy weak`, and `diag` prints `entropy  TSC + interrupts +
   carried seed` (`rdrand + carried seed` with both). Why strong: the
   seed holds everything every earlier boot gathered (each boot's cycle
   counter and the timing of its interrupts until init's late write), it
   is never used twice, and an attacker who cannot read the disk between
   boots cannot know it. An attacker who can read the stick (physical
   access) can read it, as he can read sshd's host key and `random_seed`
   next to it: the seed is exactly as protected as the keys it serves.
   The weak case stays visible: the first boot of a disk, or a boot whose
   store was blank, unreadable or not rewritten, reports weak as before,
   now with the reason (`entropy  TSC + interrupts (N samples, M events):
   weak, no carried seed (first boot)`; netd's and sshd's weak lines name
   it too). The first carried boot carries only what the first boot
   gathered; nothing measures that, as nothing measures the interrupt
   pool (ADR 0039), and it is accepted.
6. **Faults are recorded, secrets are not.** init reads `RANDOM_SOURCE`
   once the system disk answers and records `random seed missing` (code 2
   on a blank store, the first boot; code 0 when there is no store at
   all), `random seed unreadable` (3) or `random seed not saved` (4: the
   loader could not write the successor), each with a console line
   `AriInit: fault ...`; a failed write of its own is `random seed not
   saved` too. AriBoot logs its own line (`seed: ...`). No line, event or
   report holds the seed or anything derived from it: lines show the
   generation only, `Record`'s `Debug` omits the seed, and the runner's
   check never prints it. `diag` reads only `RANDOM_SOURCE`.
7. **A/B slots and updates.** `ARIBOOT` is outside the slots, so an
   update, a rollback or an ESP fallback keeps the seed, and the seed is
   taken before the slot policy runs, whichever slot boots. Update images
   are slot containers (`SYSA`/`SYSB`) and the updater writes only sector
   0 of `ARIBOOT`: they never carry a seed.
8. **Tests.** Host: the record round-trips and every damaged byte is
   refused, `pick` prefers the newest intact copy and tells blank from
   unreadable, `plan` writes the other copy, a model disk with a crash at
   every step never leaves a used seed, `split` depends on seed and noise
   (`libs/ab`); `BootInfo`'s field is appended and gated by `size`
   (`boot-protocol`); `RandomSource` encodes the byte, decodes 32-, 40-
   and longer replies, and `is_strong`/`summary` give the wording above
   (`sdk/abi`); images ship no seed and the store is found by GPT name,
   and the runner's `seed_store` check (`tools/xtask`). QEMU:
   `update.readonly_no_pending_boot` checks that a blank store stays
   blank when the loader cannot write (`seed_store = "absent"`), and
   `crypto.carried_seed` boots twice on one disk: boot 1 is weak with
   `random seed missing (code 2)` in `diag` and writes generations 1 and
   2; boot 2 takes generation 2, saves 3, mixes it in, reports netd's
   secret strong and `entropy  TSC + interrupts + carried seed`, writes 4
   and 5; after each boot the runner reads the disk image and requires
   the stored seed to have changed (`seed_store = "changed"`).
   `crypto.random` and `crypto.random_rdrand` show the status in the
   self-test line.

## Consequences

- Every QEMU scenario's first boot on a fresh disk records `random seed
  missing (code 2)`, and `esp-only` disks `random seed missing (code 0)`:
  the truth about those disks, like `net tcp secret weak`.
- A boot now writes six sectors of `ARIBOOT` beside the boot state (the
  loader's successor and blank, init's two refreshes of two writes each):
  negligible wear for a stick, and on a read-only disk a `NotSaved` boot
  with its faults.
- `diag` lists the last 32 kernel events instead of 16: with init's
  `random seed missing` one more event at boot, netd's `net tcp secret
  weak` fell out of a 16-event window before the owner could read it
  (`ssh.entropy_weak` caught it).
- init's two refreshes are block requests at boot. Scenarios that inject
  a USB fault on "the first block request after two idle seconds"
  (`usb_fault=idle`) rely on init's late refresh coming less than two
  seconds after the early one (the network wait in between is at most
  2 s on virtio-net); they pass, but the coupling is noted here in case
  one of them turns flaky (incident 0019 rule: fix or `serial`, never
  retry).
- The verdict is about the disk, not the machine: a VM booted from a
  snapshot (`snapshot=on`) or a cloned disk image reads the same seed on
  every boot and still reports it as carried. The mixing stays safe (the
  cycle counter and interrupts are added), only the verdict is wrong;
  `cargo xtask` images avoid it by shipping none.
- Not done: AriBoot does not ask the firmware's `EFI_RNG_PROTOCOL`, which
  some machines without `rdrand` provide and which would make even the
  first boot strong; nothing saves the seed at `shutdown` or `reboot`
  (the shell powers off itself) or refreshes it during a long uptime;
  the laptop has not yet been read with this build (`docs/hardware.md`:
  expected first boot weak, then `entropy  TSC + interrupts + carried
  seed`). No QEMU scenario covers `NotSaved` or `Unreadable` end to end
  (a disk variant that ships a seed or a damaged store would), the USB
  storage path of init's refresh, or a power cut between AriBoot and
  init; the host model in `libs/ab` covers the crash points.
