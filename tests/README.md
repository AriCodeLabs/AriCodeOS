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
                         those the shell launches with capabilities (`run tcp_hog`, `run sock_leaker`)
```

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
(README.md, "Docker"), which gives 10 and 6 jobs, and one suite runs at a
time. With several jobs the scenarios that run alone go first, then the
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
  the same network namespace stops at once with a message saying so. Run
  concurrent suites in separate containers (README.md, "Docker"): each has
  its own network namespace.

`serial = true` makes a scenario run with nothing else at the same time.
Only `usb.hid_fault` has it: its keys are paced in wall-clock time while the
guest drains QEMU's 16-scancode usb-kbd queue at its own pace, and with
other QEMU runs on the host it fell behind and lost keystrokes (incident
0019). A scenario that turns out to depend on timing under load is a bug to
fix or, when the dependency is the test's own, to mark `serial` with the
reason in its file, never a retry.

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
