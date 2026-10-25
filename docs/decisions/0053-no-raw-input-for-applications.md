# ADR 0053: Applications never get the raw input services

- **Date:** 2026-10-25
- **Status:** Accepted (failure-pattern audit 2026-10-25, info: the
  `input` capability; amends ADR 0013 point 4)

## Context

A manifest could ask for `input` and the broker handed the program a send
handle to every input service (`inputd`, `usbd`): raw evdev events from
the physical keyboard and mouse. ADR 0013 point 4 accepted that such a
program competes with the console bridge for key presses. The audit read
it as a keylogger door: a signed app reads what the user types into the
shell, into an SSH password prompt typed on the console, or, with a GUI,
into another program's window, bypassing the focus the compositor keeps
(ADR 0052). No application or test program asked for it.

## Decision

1. `input` is no longer in `Caps::APP_ALLOWED`, nor on any trust list.
   A manifest asking for it is denied like `spawner`, `pci` or
   `framebuffer`, and init says why (`AriInit: denied, requests system
   capabilities: input`).
2. The raw input services go only where init sends them: the console
   bridge in shell mode, the compositor with a GUI, and init's own test
   programs. A program gets keys through the console it runs on or
   through its window's `Poll`.
3. The broker no longer keeps the input endpoints.

## Consequences

- An application that needs keys runs in the console or opens a window.
  A future need for raw devices (a game controller, say) gets its own
  decision rather than a general grant.
- `sandbox.manifest` checks the denial with `input_client`'s test
  manifest.
