# Failure protocol

How AriCodeOS and the people and agents working on it treat anything that
goes wrong. Adopted 2026-10-12 after the USB stick incident
(`docs/incidents/0001-usb-stick-transaction-errors.md`), when a transfer
error was first answered with retries and pauses instead of with an
investigation. ADR 0028 records the decision; this file is the procedure.

## Principles

1. **Failures are never hidden.** Every anomaly a component sees (a failed
   transfer, a rejected command, a timeout, a checksum mismatch, a retry)
   is reported where it can be seen later: the console, the kernel event
   ring (`diag`) and, when a disk works, `diag NAME` in AriFS. A retry
   that succeeds is still a failure that happened.
2. **Nothing is assumed.** A workaround (retry, pause, reset, bigger
   timeout) is not a fix. It may be kept only when the device or protocol
   specification says the behaviour is expected, and then the
   specification is cited next to the code. Otherwise the failure gets an
   incident and an investigation.
3. **Stop instead of looping when debugging.** With `strict` on the boot
   command line, the first unexpected failure halts the component that saw
   it: it dumps its state, records a fault event and exits; init does not
   restart anything in strict mode. The system stays up for inspection
   (console, `diag`, SSH when the network does not depend on the failed
   part). Without `strict`, retries are bounded, counted and recorded, and
   a component that exhausts them fails loudly.
4. **Everything is recoverable.** Diagnostics survive the session: `diag
   NAME` saves the report to AriFS, `cargo xtask fs-cat` reads it from the
   stick on the host, and the incident file keeps photos, logs and
   numbers. A fault without evidence is reopened, not closed.
5. **The smallest failure is fixed.** An intermittent error, a flaky test,
   a message that appears once: each one gets a cause or an open
   incident. "It works now" is not a resolution.
6. **Doubt triggers the improvement protocol.** When the cause is not
   known with evidence, several agents study the suspected critical point
   independently, from different angles, before any change. A fix needs a
   reproduction or a measurement that confirms the mechanism; a change
   that merely makes the symptom go away is reverted.

## Procedure

1. **Record.** Open `docs/incidents/NNNN-slug.md` with: date, hardware or
   environment, exact messages (photos transcribed), timeline, what was
   running, commits involved. Add every new observation there, including
   dead ends.
2. **Reproduce or instrument.** Prefer a reproduction in QEMU or on the
   host (unit test, scenario). If the failure only shows on hardware, add
   instrumentation that prints the facts needed (codes, addresses,
   counters, timings) and boot with `strict` so the first failure stops
   the component with its state intact.
3. **Study.** Launch at least three independent reviews with different
   angles (the hardware specification, the protocol specification, the
   recent changes in the repository). Each produces ranked hypotheses with
   code lines as evidence and the instrumentation that would confirm or
   refute them. The reports are attached to the incident.
4. **Confirm.** Run the instrumentation; keep only the hypotheses the
   evidence supports. A hypothesis nobody could test stays listed as open.
5. **Fix.** Change the code for the confirmed mechanism, with a test that
   fails before and passes after (unit, scenario or host test). Cite the
   specification where the behaviour comes from. Workarounds that remain
   are documented as such, bounded, counted and visible in `diag`.
6. **Audit.** Before merging, a review agent checks the fix against the
   incident: does it address the confirmed mechanism, does it hide
   anything, are the logs still complete. Larger changes get the full
   audit (`docs/audits/`).
7. **Close.** The incident file ends with the cause, the fix commit, the
   evidence and what remains open. The ADR of the affected subsystem gets
   a note.

## Mechanisms in the system

- `strict` on the command line (`init_arg::STRICT`, `driver_arg::STRICT`
  for drivers): halt on the first failure, no restarts by init.
- `sys::event_record(code, name)`: any program records a fault in the
  kernel event ring; `diag` lists it with the process and the time.
- Bounded retries: a component may retry a bounded number of times, each
  attempt recorded as a fault event with the reason code; the last one is
  reported as an error.
- `diag [NAME]`: full report with the events, saved to AriFS when a name
  is given; `cargo xtask fs-cat IMAGE NAME` reads it on the host.
