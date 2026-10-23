# AriCodeOS tests

All tests live in the repository and run with `cargo xtask test`.

## Levels

| Level   | What it runs                                              | Duration | When it is mandatory                    |
|---------|-----------------------------------------------------------|----------|-----------------------------------------|
| `unit`  | `cargo test` of the host crates (`libs/*`, protocols, xtask) | seconds | on every commit of a feature            |
| `smoke` | unit + nominal boot in QEMU (`boot.kernel_ready`)          | ~5 s     | before merging into `develop`           |
| `full`  | unit + every QEMU scenario (provoked faults included)      | minutes  | before merging into `main` (`--all`)    |

```bash
cargo xtask test                    # unit + smoke (default)
cargo xtask test --level unit       # host only
cargo xtask test --all              # everything: mandatory towards main
cargo xtask test --all --jobs 8     # 8 QEMU scenarios at a time (see "Parallel runs")
cargo xtask test --area boot        # QEMU scenarios of one area (with --all to include the full ones)
cargo xtask test --list             # list scenarios and which ones the filter selects
```

## Where things are

```text
libs/*/src/lib.rs        #[cfg(test)] unit tests of ELF, paging, cmdline, AriFS, WASM, ...
boot-protocol/src/lib.rs #[cfg(test)] memory classification, iterator, cmdline
tools/xtask/src/*.rs     #[cfg(test)] scenario validity, helpers
tests/qemu/*.toml        integration scenarios in QEMU
user/testprogs/src/bin/  test programs in the initrd; user/testprogs/manifests/ holds the manifest of
                         those the shell launches with capabilities (`run tcp_hog`, `run sock_leaker`,
                         `run tcp_orphan`)
```

## Stack budget of every initrd program

`cargo xtask check` ends with `cargo xtask stack-check` (incident 0018),
which can also run alone. It builds every initrd program (services,
applications, test programs: 63 binaries) in the release and the dev
profile, disassembles each with binutils' `objdump`, measures every
function's frame from its prologue (return address, pushes, `rsp`
adjustment, stack probes) and the deepest chain of direct calls from
`_start` (calls, unconditional and conditional tail calls), and fails
when, in any binary:

- the deepest direct path is over its path budget, by default **32 KiB**,
  half the 64 KiB user stack (`USER_STACK_SIZE` in
  `kernel/src/user/process.rs`; a unit test fails if that constant
  changes), or
- one frame is over its frame budget, by default **16 KiB**, a quarter of
  the stack.

A program allowed more goes in `stack::EXCEPTIONS` with its budget, the
reason and the incident that tracks it (a unit test requires the
incident); the table is empty: every offender found so far was fixed. The
report shows the network drivers, the three binaries closest to their
budget and every one over it, with the deepest path and the largest frames
function by function.

The measured depth is a lower bound. Not counted: indirect calls (trait
objects, `core::fmt` callbacks, function pointers end a path), recursion
(direct or mutual: the call back into the chain is cut; direct recursion
is noted), `rsp` adjustments after a function's first call and stack
realignment (`and $-N,%rsp`). The other half of the stack is for those and
for growth. Functions are keyed by address, so two with the same demangled
name are both measured. The parser and the budgets are tested on the host
(`tools/xtask/src/stack.rs`).

The run-time counterpart is the kernel's high-water mark (`diag`, `user
stacks`), and scenario `network.deep_path` drives netd's deepest path (a
full TCP table swept inside a request, a dead client's reset toward an
address without ARP); with `cargo xtask test --debug --all --area network`
it runs on a dev build, where the margin is smallest.

## Writing a QEMU scenario

A file `tests/qemu/<area>_<name>.toml`:

```toml
name = "area.name"              # unique; only [a-z0-9._-]; also the directory under build/scenarios/
description = "one line"
level = "smoke" | "full"
areas = ["boot", "memory"]      # to filter with --area
cmdline = "qemu_exit ..."       # written to aricodeos/cmdline.txt, with " selftest" appended (below)
selftest = true                 # default: init also runs its test clients (they write to the test disk and talk to
                                # the test hosts at 10.0.2.2); false boots like a real machine, health checks only
                                # (ADR 0032; init.no_selftest, init.no_selftest_usb, usb.image_boot, update.fs_nearly_full,
                                # update.fs_one_short)
timeout_secs = 30
stdin = ["ls", "shutdown"]      # optional: lines the runner types on the serial console
prompt = "ari> "                # each line is sent when the prompt appears once more (default "ari> ")

[host_tcp]                      # optional: TCP probe from the host into the guest (hostfwd 127.0.0.1:<port of the run> -> :7)
marker = "TCP_LISTEN_READY"     # when seen on the serial, xtask connects and sends payload
payload = "hello from the host" # the echo is recorded in the capture as "[host] TCP probe: ..."

disk = "slots"                  # disk: slots (default) | esp-only | pending-b | b-exhausted | b-corrupt | usb-image (the ESP lives inside the disk)
                                #       | fs-pending-replace (an AriFS replace committed but not applied) | fs-corrupt (AriFS table altered)
                                #       | wasm-restricted (module manifest with print only) | wasm-invalid (ill-typed demo.wasm)
                                #       | wasm-bad-import (demo.wasm imports a host function with another signature)
                                #       | a-corrupt (slot A fails verification: ESP boot, installed version unknown)
                                #       | pending-b-older-update (B v2 pending, UPDATE holds v1) | fs-nearly-full (5 entries free for the updater) | fs-one-short (4)
disk_readonly = false           # true: the test disk is presented read-only
serial = true                   # runs with no other scenario at once (--jobs); say why in the file (tests/README.md, "Parallel runs")
cpus = 4                        # processors (-smp N); undeclared: 1, or whatever `--cpus N` forces
cpu_rdrand = true               # -cpu qemu64,+rdrand (TCG implements it): the kernel generator is seeded from rdrand (ADR 0039);
                                # default qemu64 has none, so every other scenario runs the weak TSC + interrupts path
usb_hid = true                  # adds qemu-xhci with a USB keyboard and mouse (QMP events go to the USB devices)
usb_hid_only_input = true       # no virtio keyboard/mouse, so QMP input reaches the USB HID devices (xHCI or EHCI)
usb_hid_msi = true              # that xHCI offers MSI only, no MSI-X (like 2012 Intel controllers)
usb_storage = true              # the test disk hangs off the xHCI as USB mass storage; there is no virtio-blk
host_https = true               # two host HTTPS servers (openssl s_server -www) on ports chosen per run, written {https_port} (cert from a
                                # test CA installed as trusted_ca.der) and {https_untrusted_port} (self-signed) in stdin, kill_on and expect

[host_ssh]                      # SSH from the host (ADR 0025): a client key is generated and installed as authorized_keys;
marker = "sshd: listening on port 22"   # on seeing the marker, each command runs with the host's ssh through
commands = ["ls", "shutdown"]   # host port of the run forwarded to 22; output joins the log as "[host] ssh $ CMD", "[host] ssh| LINE", "[host] ssh exit N"
idle_client = true              # a TCP client that never identifies itself holds port 22 first; sshd must drop it
[host_flood]                    # connection flood (incident 0013): on seeing marker, `connections` plain TCP connections to the
marker = "sshd: listening on port 22"   # guest's `port` (22, default, or 7: the ports with a hostfwd) are opened, never written to and held
port = 22                       # until QEMU exits; the console lines are typed only once they are open and watched for 2 s. QEMU
connections = 8                 # completes the handshakes itself, so the guest sees established connections nobody accepts, not
                                # half-open ones. Port 22 installs the scenario's SSH key so sshd listens. Result lines: "[host] flood:
                                # N connections to guest port P opened", "[host] flood: K of N connections reset by the guest within 2000 ms"
[host_update]                   # an update source (ADR 0029) published in build/scenarios/<name>/update/ and served by the scenario's own HTTP
                                # server at http://10.0.2.2:{update_port}/update/ (write the URL that way in stdin, kill_on and expect)
system = 2                      # system image version offered (omit: no system image)
apps = ["examples/hello_ext:2"] # app packages offered (DIR:VERSION)
tamper_index = true             # the index is altered after signing (the updater must refuse it)
tamper_image = true             # the image is altered after the index recorded its SHA-256
stale_index = true              # the machine already holds a verified copy with a higher serial
source = "sources"              # sources (installs updater/sources) | legacy-url (installs the ADR 0027 updater/update_url) | none
dev_build = 5                   # also a development source (ADR 0045) offering build 5, published in build/scenarios/<name>/dev/ and
                                # served at http://10.0.2.2:{update_port}/dev/, signed with the development channel's test key
dev_commit = "0123abc"          # the commit that build names (default 0123abc)
dev_wrong_key = true            # the development source is signed with the stable test key (the updater must refuse it)
dev_source = true               # register it in updater/sources as `dev URL` (default: not registered, the channel is closed)
usb_legacy_controller = true    # also adds an EHCI with no devices (the EHCI driver runs next to usbd)
usb_ehci_hid = true             # an EHCI with a USB keyboard and mouse (high speed) for usb_ehci (ADR 0031)
usb_ehci_storage = true         # the test disk hangs off the EHCI as USB mass storage; there is no virtio-blk
usb_ehci_companion_kbd = true   # an ICH9 EHCI with a UHCI companion and a full-speed USB keyboard (port handover)
no_nic = true                   # no network card: init runs without a network service (the capture still ends with the frame count)
flr_device = true               # adds an unused igb NIC that advertises Function Level Reset (test=pci_reap, incident 0004)
install = ["examples/hello_ext"]  # external apps xtask builds, signs (version 1) and installs in AriFS as apps/NAME.aripkg
packages = ["examples/hello_ext:2", "examples/hello_ext:3:corrupt"]  # pending packages in pkgs/ (DIR:VERSION[:corrupt]) for `run installer`
staged = ["examples/hello_ext:2:wrong-hash"]  # packages already staged for `run installer 2` (DIR:VERSION[:wrong-hash])
staged_bad_path = true          # adds a staged line naming pkgs/evil.aripkg

[host_input]                    # optional: input injected through QMP when marker is seen
marker = "INPUT_READY"
keys = ["h", "o", "l", "a", "ret"]   # QEMU qcode names, press and release on kbd0; "shift+dot" holds the modifiers while the key is pressed
mouse_rel = [10, -5]            # relative movement on mouse0
click = "left"                  # button pressed and released; result in "[host] QMP: ..."
key_delay_ms = 30               # minimum pause after each key event (default 0)
echo_prompt = "ari> "           # the guest prints this before each line and echoes typed characters: each line waits for it and each
                                # key but the last for its echo (10 s of QEMU running at most, a thermal pause not counted, then
                                # "[host] QMP: failed after N events (no echo of 'd' ...)"). Required when more than 16 key codes
                                # (PS/2 bytes) would go out between two waits: QEMU's usb-kbd and i8042 queue 16 (incident 0022)

[inject]                        # optional: provoked faults in the ESP
truncate_kernel = 3000          # truncates kernel.elf to N bytes
remove_kernel = true            # removes kernel.elf
patch_entry = 0                 # overwrites the ELF's e_entry
add_empty_segment_at_entry = true   # adds an empty RW PT_LOAD (memsz 0) at e_entry

[expect]
exit = 33                       # 33 = success, 35 = failure reported by the system, 0 = reboot (-no-reboot)
serial_contains = ["..."]
serial_absent = ["..."]         # every capture ends with "[host] guest frames other than ARP and DHCP: N", counted in the
                                # QEMU filter-dump capture of the guest network (net.pcap in the scenario directory)
serial_order = ["...", "..."]   # these texts appear in this order (each after the previous one's match)
seed_store = "changed"          # optional: the kernel seed in the test disk's ARIBOOT after the boot (ADR 0043):
                                # `absent` (no intact copy) or `changed` (one that differs from the disk's before
                                # this boot; a fresh disk holds none). The runner reads it from disk.img and never prints it
```

A scenario with several boots on the same disk declares `[[boots]]` instead
of `cmdline`, `stdin` and `[expect]` (the test disk is built once and kept;
every boot has its own ESP and its `serial-N.log`):

```toml
[[boots]]
cmdline = "qemu_exit shell"
stdin = ["run updater", "reboot"]
kill_on = "updater: image in UPDATE version 2 verified ("   # optional: power cut (kills QEMU when the text appears)
[boots.expect]
exit = 0                        # or `killed = true` when the cut is expected
serial_contains = ["..."]

[[boots]]
cmdline = "qemu_exit"
[boots.expect]
exit = 33
serial_contains = ["AriInit: boot confirmed in slot B"]
```

Provoked faults inside the kernel are requested on the command line
(`test=panic`), never through compile-time features: the same binary serves
every scenario and no test code is scattered behind `cfg`. Ring 3 programs
(`test=user_<name>`) live in `user/testprogs` as their own binaries (Rust on
`aricode-sys`, or assembly) and return an exit code that identifies the
failed step.

Note that the strings the scenarios expect are the system's console
messages, so a change to a message must update the scenarios that assert on it.

## Areas

Every roadmap phase adds its area and its scenarios. Existing ones: `boot`,
`panic`, `console`, `interrupts`, `memory`, `timer`, `threads`, `scheduler`,
`user`, `capabilities`, `ipc`, `init`, `drivers`, `acpi`, `pci`, `virtio`,
`storage`, `filesystem`, `shell`, `network`, `input`, `gui`, `sandbox`,
`wasm`, `crypto`, `update`, `smp`, `iommu`, `usb`, `hardware`, `services`,
`limits`, `apps`, `perf`, `diag`. A branch `feature/f05-physical-memory`
runs `--area memory` while developing and `--all` before integrating. If the
filter selects no QEMU scenario, the runner fails and lists the known areas:
a misspelt filter never produces an empty success.

## Several CPUs

Scenarios boot with one CPU unless they declare `cpus = N`; the Phase 32
ones use 4 (and `cpus = 1` the one that tests degradation to a single CPU).
`cargo xtask test --all --cpus 4` boots every scenario that does not declare
`cpus` with four processors: it is the SMP stress test of the kernel and must
pass in full before integrating changes to the scheduler, the locks or the
ring 0 entry paths. Self-tests with single-CPU scheduling expectations
(`scheduler.selftest`, `threads.selftest`) pin their threads to CPU 0 so they
hold with any `--cpus`.

## Parallel runs

`cargo xtask test --jobs N` runs N QEMU scenarios at a time (ADR 0040).
Without `--jobs` the runner takes one run per CPU it may use, at most 10,
and two thirds of that with `--cpus` above 1, at most 6 (each such guest
keeps several host threads busy under TCG); never more runs than the
available memory holds at 1 GiB each (`MemAvailable`, or the container's
`--memory` limit, which also caps an explicit `--jobs`). `--jobs 1` runs
them one after another in listed order as before. On the reference host
(16 CPUs) test containers get `--cpus 10 --memory 12g --memory-swap 12g`
(README.md, "Docker"), which gives 10 and 6 jobs. With several jobs the scenarios that run alone go first, then the
longest (the time of their previous run, kept in
`build/scenarios/<name>/elapsed`), but the output is the same whatever
finished first: each scenario's block (its PASS/FAIL line, the log of a
failure, QEMU's own messages) is printed in listed order, then the wall
clock and the usual summary line and exit code.

What keeps concurrent runs apart:

- **Work directory**: each scenario writes only under
  `build/scenarios/<name>/` (disk, ESP, OVMF variables, logs, QMP socket,
  TLS and SSH keys, and its update source in `update/`). The base ESP and
  the external packages are built once before the scenarios start.
- **Host ports chosen per run**: QEMU's two `hostfwd` ports (the
  `[host_tcp]` probe, `[host_ssh]` and `[host_flood]`), the HTTPS servers
  of `host_https` and the HTTP server of `[host_update]`. The guest reaches
  a host port P at 10.0.2.2:P, so scenarios write those ports as
  `{https_port}`, `{https_untrusted_port}` and `{update_port}` and the
  runner fills them in; a placeholder without its server is refused when
  the scenarios load.
- **Fixed ports, shared by the suite**: the UDP responder (17007), the TCP
  echo server (17008) and the test HTTP server (17010) are compiled into the
  guest's test programs (`aricode_proto::net::TEST_*`), and QEMU's user
  network cannot remap them (`guestfwd` refuses the virtual host 10.0.2.2,
  and has no UDP). They are stateless, so one set serves every scenario of
  the suite; it is taken before anything is built, and a second suite in
  the same network namespace (which the suite lock below already holds
  back) would stop at once with a message saying so.

**One suite at a time on the host.** Before its first scenario, `cargo
xtask test` (any level but `unit`) takes an exclusive `flock` on
`ARICODEOS_SUITE_LOCK`, by default `$HOME/.cache/aricodeos-lock/suite.lock`
(`tools/xtask/src/suite_lock.rs`), and holds it until it exits. A second
suite, on the host or in any container, waits and prints the holder:

```text
[xtask] waiting for another QEMU suite on this host (lock /home/u/.cache/aricodeos-lock/suite.lock; holder: pid 4242; host 3f2c1a9b (container); checkout /home/u/AriCodeOS; head 1a2b3c4; started 2026-10-23T02:00:00Z; suite cargo xtask test, level Full, --cpus 4)
```

Containers mount the host's directory at `/run/aricodeos-lock` (the
development image sets `ARICODEOS_SUITE_LOCK` there; README.md, "Docker").
Inside a container without that mount a QEMU suite refuses to start: its
lock would be private to the container and exclude nothing. The CI
container takes the same lock with `flock` before its first test pass and
releases it after the last one, and its passes skip taking it again
(`ARICODEOS_SUITE_LOCK_HELD`). The lock is the kernel's: it goes away with
the process that held it, however it ended.

`serial = true` makes a scenario run with nothing else at the same time.
`network.deep_path` has it (the reason is in its file). `usb.hid_fault` had
it while its keys were paced in wall-clock time (incident 0019), and still
lost a keystroke running alone on a loaded host; the runner now types each
key after the guest's echo of the previous one (`echo_prompt`, incident
0022), which does not depend on host load. A scenario that turns out to
depend on timing under load is a bug to fix or, when the dependency is the
test's own, to mark `serial` with the reason in its file, never a retry.

## Continuous runs (Docker)

`tools/docker/ci/` holds a container that tests a branch (default
`develop`) on every new commit, in its own clone inside a named volume,
and writes dated reports to a mounted directory (README.md, "Docker"). A
failure there is an incident (docs/WORKFLOW.md).

## Logs

Every scenario leaves its serial output in `build/scenarios/<name>/serial.log`
(QEMU's own messages in `qemu-stderr.log`).
The runner refuses to work if that directory or its `esp` are symbolic links
or if the canonical path escapes `build/scenarios/`.
