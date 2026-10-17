# Evidence for the 525b550 release review

See [the audit](../2026-10-18-release-0.11.0-525b550.md).

The official logs come from two isolated archives of commit
`525b55011212ad01195c91d5d997663129a214bc`. `port-remap.txt` describes the
only source differences in the four-CPU copy. Both full runs exited 0.

## Reproduce the additional findings

Use a checkout or archive of the reviewed commit as SOURCE, with its
pinned Rust toolchain available:

```bash
python3 docs/audits/525b550-review/probe.py SOURCE /var/tmp/aricode-review-probes
```

The script writes an independent Cargo project to the output directory,
extracting `file_size`, `read_all`, `write_file`, `read_state`,
`write_state_section` and `Ehci::scsi` verbatim from SOURCE. The production
HTTP and update-index libraries are used as path dependencies.

`installer.rs.in` supplies a filesystem mock that fails the first list
request, then succeeds. `ehci.rs.in` supplies a USB bulk mock whose CSW
transfer writes the signature and current tag but returns only eight
bytes. `http.rs.in` supplies a simulated clock and one-second interim
responses; its sink uses the updater's deadline condition.

Expected on the reviewed code: **three assertion failures, exit 101**.
These are assertions of required behaviour that the implementation
violates, not compilation failures. `probes.log` records the final run.
The generated `src/lib.rs` remains available for inspection; the script
does not modify SOURCE or fix production code.

Boundaries: these are host integration probes, with filesystem and USB
I/O simulated. They do not claim a real malformed USB-device execution
or ten minutes of wall-clock network traffic. The full official QEMU
suite is recorded separately in `full-1.log` and `full-4.log`.
