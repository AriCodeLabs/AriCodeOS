# ADR 0028: Failure protocol, strict mode and fault events

- **Date:** 2026-10-12
- **Status:** Accepted

## Context

The first intermittent hardware failure (incident 0001, USB stick
transaction errors on the laptop) was answered with retries, pauses and a
device reset chosen by guesswork; one of them made the device stop
answering altogether, and `sshd` refused connections because a failed read
looked like an empty file. The owner's rule: failures are not hidden, the
smallest one is corrected, nothing is assumed, doubt triggers an
investigation by several agents, and while debugging the system stops at
the first failure instead of looping.

## Decision

1. **A written procedure** (`docs/FAILURE-PROTOCOL.md`): record the
   incident, reproduce or instrument, independent studies from several
   angles, confirm with evidence, fix with a test, audit, close. Workarounds
   that remain are bounded, counted, visible and documented with the
   specification that justifies them.
2. **`strict` boot flag** (`init_arg::STRICT`, passed to drivers as
   `driver_arg::STRICT` in their argument): a component halts on its first
   unexpected failure after dumping its state and recording a fault event;
   init restarts nothing in strict mode and says so.
3. **Fault events**: the `event_record` system call lets any program put a
   `Fault` entry (code and a short name) in the kernel event ring; `diag`
   lists them with the process and the time, and `diag NAME` saves them to
   AriFS. Drivers record every transport failure, including retried ones.
4. **Incidents directory** (`docs/incidents/`): one file per failure with
   evidence, hypotheses, study reports and resolution; it is part of the
   repository.

## Consequences

- Strict mode is a debugging aid: a system booted with it may lose its
  disk or network on the first fault by design.
- The event ring is bounded (oldest entries dropped); long sessions should
  save `diag` reports periodically.
- Retries in the USB storage driver remain bounded and recorded until
  incident 0001 establishes the cause.
