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
cargo xtask test --area boot        # QEMU scenarios of one area (with --all to include the full ones)
cargo xtask test --list             # list scenarios and which ones the filter selects
```

## Where things are

```text
libs/*/src/lib.rs        #[cfg(test)] unit tests of ELF, paging, cmdline, AriFS, WASM, ...
boot-protocol/src/lib.rs #[cfg(test)] memory classification, iterator, cmdline
tools/xtask/src/*.rs     #[cfg(test)] scenario validity, helpers
tests/qemu/*.toml        integration scenarios in QEMU
```

## Writing a QEMU scenario

A file `tests/qemu/<area>_<name>.toml`:

```toml
name = "area.name"              # unique; only [a-z0-9._-]; also the directory under build/scenarios/
description = "one line"
level = "smoke" | "full"
areas = ["boot", "memory"]      # to filter with --area
cmdline = "qemu_exit ..."       # written to aricodeos/cmdline.txt
timeout_secs = 30
stdin = ["ls", "shutdown"]      # optional: lines the runner types on the serial console
prompt = "ari> "                # each line is sent when the prompt appears once more (default "ari> ")

[host_tcp]                      # optional: TCP probe from the host into the guest (hostfwd 127.0.0.1:17009 -> :7)
marker = "TCP_LISTEN_READY"     # when seen on the serial, xtask connects and sends payload
payload = "hello from the host" # the echo is recorded in the capture as "[host] TCP probe: ..."

disk = "slots"                  # disk: slots (default) | esp-only | pending-b | b-exhausted | b-corrupt | usb-image (the ESP lives inside the disk)
                                #       | fs-pending-replace (an AriFS replace committed but not applied) | fs-corrupt (AriFS table altered)
                                #       | wasm-restricted (module manifest with print only) | wasm-invalid (ill-typed demo.wasm)
                                #       | wasm-bad-import (demo.wasm imports a host function with another signature)
disk_readonly = false           # true: the test disk is presented read-only
cpus = 4                        # processors (-smp N); undeclared: 1, or whatever `--cpus N` forces
usb_hid = true                  # adds qemu-xhci with a USB keyboard and mouse (QMP events go to the USB devices)
usb_hid_msi = true              # that xHCI offers MSI only, no MSI-X (like 2012 Intel controllers)
usb_storage = true              # the test disk hangs off the xHCI as USB mass storage; there is no virtio-blk

[host_ssh]                      # SSH from the host (ADR 0025): a client key is generated and installed as authorized_keys;
marker = "sshd: listening on port 22"   # on seeing the marker, each command runs with the host's ssh through
commands = ["ls", "shutdown"]   # host port 17022; output joins the log as "[host] ssh $ CMD", "[host] ssh| LINE", "[host] ssh exit N"
idle_client = true              # a TCP client that never identifies itself holds port 22 first; sshd must drop it
host_https = true               # two host HTTPS servers (openssl s_server -www) on 17443 (cert from a test CA installed as trusted_ca.der) and 17444 (self-signed)
host_update = true              # publishes a version-2 system image in build/update/, served at http://10.0.2.2:17010/update/ (ADR 0027), and installs updater/update_url
usb_legacy_controller = true    # also adds an EHCI with no devices (the EHCI driver runs next to usbd)
usb_ehci_hid = true             # an EHCI with a USB keyboard and mouse (high speed) for usb_ehci (ADR 0029)
usb_ehci_storage = true         # the test disk hangs off the EHCI as USB mass storage; there is no virtio-blk
install = ["examples/hello_ext"]  # external apps xtask builds, signs (version 1) and installs in AriFS as apps/NAME.aripkg
packages = ["examples/hello_ext:2", "examples/hello_ext:3:corrupt"]  # pending packages in pkgs/ (DIR:VERSION[:corrupt]) for `run installer`

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
serial_absent = ["..."]
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

## Logs

Every scenario leaves its serial output in `build/scenarios/<name>/serial.log`.
The runner refuses to work if that directory or its `esp` are symbolic links
or if the canonical path escapes `build/scenarios/`.
