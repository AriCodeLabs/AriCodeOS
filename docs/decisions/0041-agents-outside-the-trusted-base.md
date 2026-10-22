# ADR 0041: Agents work on AriCodeOS from outside the trusted base

- **Date:** 2026-10-23
- **Status:** Proposed (draft, recorded so the idea is not lost). The
  owner's priority stays a system that is tightly bound and secure; no
  work on this starts before the open security and robustness items are
  closed.

## Context

AriCodeOS has been built largely with AI agents working under the owner's
direction: implementation, independent audits, reproduction before every
fix, the failure protocol (ADR 0028). That experience suggests a direction
other systems may not have taken from their base: services that are more
intuitive to use and to diagnose, with agents helping the owner instead of
a rigid set of commands only.

The risk is equally clear. A language model is not deterministic, can be
wrong, and can be steered by hostile text (prompt injection). Anything it
decides inside the kernel or a core service would undo the guarantees the
project has built: capabilities, verified updates, bounded resources,
refusals logged, faults never hidden.

## Decision (proposed)

1. **No model in the trusted base.** The kernel, init, the drivers and the
   core services (fsd, netd, sshd, the updater) stay deterministic. No
   model decides memory, access, scheduling or what gets installed.
2. **An agent is an ordinary process with capabilities.** It receives only
   the handles its manifest declares (ADR 0016's model), granted by the
   owner, revocable, and every grant and refusal recorded. It gains no
   authority from what it reads: text from the network, files or logs is
   data, never instructions to the system.
3. **Agents propose, the system and the owner decide.** Anything that
   changes the machine (installing, configuring, deleting, rebooting)
   goes through the existing typed operations, shown to the owner as a
   plan with the capabilities it needs, and runs only after explicit
   confirmation; read-only diagnosis needs no confirmation.
4. **Everything reversible and audited.** Changes go through the paths
   that already guarantee this (A/B updates, atomic replace, signed
   packages); every action an agent triggers is a recorded event.

## Directions this enables (not committed)

- **Structured observability.** `diag`, the fault events, the incidents
  and the TCP/descriptor tables exposed as typed data, so a diagnosis
  agent can read the machine's state and propose an explanation or a fix.
  This is what was done by hand with the reference laptop.
- **An intent shell.** The owner states what he wants in plain language;
  an agent translates it into concrete shell operations, shows the plan,
  and runs it only on confirmation. Translation is flexible; execution
  stays deterministic.
- **Agent-native capability brokering.** Because capabilities are the base
  of AriCodeOS rather than an added sandbox, an agent can be given exactly
  "read the logs" or "install this signed package", which is hard to do
  safely on systems where a process inherits its user's authority.
- **The method as a product.** The failure protocol with independent
  audits and reproduction before fixing, run with agents, is itself worth
  describing.

## Constraints

- The reference laptop has 4 GiB of memory and no hardware entropy: a
  local model does not fit there. A first agent would run on another
  machine and talk to AriCodeOS over SSH with a restricted capability set.
- Prompt injection is the main threat: an agent that reads hostile text
  must not be able to widen its own capabilities; grants come only from
  the owner.

## First step, when the time comes

A typed, versioned export of `diag` and the fault events (read-only), and
a diagnosis agent outside the machine that reads it and proposes; no
write access at all. Only after that has been audited would any
confirmed, write-capable operation be considered.

## Consequences

- Nothing changes in the code now; this records a direction and its
  limits.
- Any future work under this ADR follows the same rules as the rest of the
  project: tests in the repository, an ADR per contract change, an
  independent audit, and the owner's security rules.
