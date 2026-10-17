# Audit 1 of feature/ehci (ADR 0031)

- **Date:** 2026-10-17
- **Audited commit:** `ba407d5`
- **Verdict:** not mergeable yet

## Summary

Correct: SDK handle layout safe for all drivers; async schedule removal
through the doorbell, lost QHs and their buffers never reused; qTD/QH
overlay write order; 64-bit Appendix B layouts and CTRLDSSEGMENT; handoff
5.1 and init 4.1; split/TT encoding; BOT Reset Recovery; periodic unlink
wait.

| Id | Finding |
|---|---|
| H1 | A different stick is silently served after the served one is removed |
| H2 | `strict` dumps after the timeout recovery has run |
| M1 | The system disk choice can kill init |
| M2 | Recovery budget undercounted (up to ~94 s > 90 s) |
| M3 | A failed hub query hides failures (counts as unplugged) |
| M4 | A halted controller is not detected; some failures skip `strict` |
| M5 | Info answered before the boot enumeration (flaky scenario) |
| M6 | The scenarios test less than claimed |
| M7 | Power/CPU cost on the laptop |
| Lows | Unjustified late-advance clear; hub query before the strict dump; no boot enumeration deadline; keys not released on unplug; endpoints of failed launches not closed |

## Resolution (feature/ehci, after `ba407d5`)

Lines refer to `services/usb-ehci/src/main.rs` unless stated. Verified with
`cargo xtask check`, host unit tests and `cargo xtask build`; the QEMU
scenarios are written but not run (test ports in use).

| Id | Fix |
|---|---|
| H1 | `Served` identity (336) set by the first storage served (2103); a different device is reported, recorded (`ehci other storage`) and left unconfigured (2113); while the served one is absent every block request answers `Io` (3101) |
| H2 | `run()` reports and calls `strict_halt` on a timeout before the queue head is retired (1219) |
| M1 | init (`services/init/src/main.rs` ~2015-2040): the system disk is the first USB block service whose GPT has ARI0; others are named and skipped; with none init prints "no USB storage with an ARI0 partition" and continues |
| M2 | Request/step deadlines (`REQUEST_BUDGET_MS` 86, `with_deadline` 840): every transfer is cut to the unit's deadline, refused when it cannot start (1201, `Fail::Refused`); a Reset Recovery that does not fit is owed and runs before the next CBW (2467, `recovery_failed` 2694); budget chain 15 + 36 + 36 = 87 s < 90 s checked by `const` asserts, covering hub GetPortStatus and ClearTTBuffer |
| M3 | A failed GetPortStatus counts as connected (2346) |
| M4 | Unexpected HCHalted handled like a host system error (2238); `strict_halt` on the IAA timeout (970), stuck frame index (1044), short control reply (1391); a lost EP0 queue head halts through the IAA timeout path |
| M5 | Input Info held until the boot enumeration ends or 2.5 s pass (`info_ready` 851, main loop 3299); boot enumeration bounded to 30 s (2277) |
| M6 | `usb_fault=N|idle:csw` reads a CSW with no command, a real bulk IN STALL (2495), scenario `usb.ehci_fault_csw`; scenario `usb.ehci_companion` with `usb-kbd,usb_version=1` behind an ICH9 EHCI + UHCI companion (QEMU's companion-less `usb-ehci` refuses a full-speed device at attach) |
| M7 | Documented as a follow-up in ADR 0031 (Consequences, "Power"); not measured |
| Lows | Late async-advance clear removed (QEMU's IAA handling does not need it; the unlink clears the status before every doorbell); under `strict` the state is dumped before a hub is asked about a port (1412); boot enumeration deadline (M5); an unplugged HID gets an empty report so held keys and buttons are released (2172); init closes the endpoints of a USB driver it could not register (`services/init/src/main.rs` 1968); registered ones stay with the supervisor so its endpoint retirement covers them |
