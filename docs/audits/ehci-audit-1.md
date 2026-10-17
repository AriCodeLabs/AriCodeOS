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

## Re-audit (after `1f8aa3f`)

**Verdict:** not mergeable yet; two regressions from the fixes.

H2, M1, M3, M4 and the lows are fixed; M7 is deferred and documented. H1
is fixed for the ordinary case, with gaps (R3). M2's mechanism is correct
but its budget chain is wrong (R1). The M5 fix introduced R2. The owed
Reset Recovery is correct (BOT 5.3.4). The HCHalted exit 16 is fine,
except that the identity is lost on restart. init's ARI0 selection is
safe; on the laptop the xHCI stick wins first. `usb.ehci_companion` tests
only the "reported" path (QEMU N_CC = 0, no UHCI driver).
`usb.ehci_fault_csw` is consistent with QEMU usb-storage's STALL.

| Id | Finding |
|---|---|
| R1 | Housekeeping deadlines add up instead of nesting (5 + 15 + 36 + 36 = 92 s > 90 s); the receive loop may serve up to 8 block requests in a row |
| R2 | Held Info can miss init's readiness probe (~3 s from launch) |
| R3 | Served identity too weak (no disk GUID / serial) and lost on a driver restart |
| Minors | `scsi_failed` halts on `Refused`; `poll_hubs` marks a hub failed when its time runs out; strict halt on a timeout during unplug undocumented; init's ARI0 loop leaks a DMA buffer per candidate and conflates GPT read errors with "no ARI0"; `usb.ehci_companion` wording; QEMU HCSPARAMS/HCCPARAMS fixture comment |

### Resolution

Lines refer to `services/usb-ehci/src/main.rs` unless stated. Verified with
`cargo xtask check`, host unit tests and `cargo xtask build`; QEMU not run.

| Id | Fix |
|---|---|
| R1 | One `with_deadline(HOUSEKEEPING_MS)` (92) around `poll_hids` + `service_ports` per main loop iteration (3441); the hub poll (5 s) and an enumeration nest inside by their minimum; `const` assert `HOUSEKEEPING_MS + 2 * REQUEST_BUDGET_MS < REPLY_WAIT_MS` (95); at most one block request per iteration (3416); the at-most-two-clients assumption documented with the budget chain and in ADR 0031 |
| R2 | `started` taken before `open()` (3319); `INFO_WAIT_MS` 1.5 s (104); held Info lives in the driver (`hold_info` 889, `release_info` 901) and is answered from inside every transfer wait (1285) and pause (914) and before every hub poll and enumeration (2377, 2387) once due |
| R3 | `Served` (347) adds the GPT disk GUID read from LBA 1 (2192) and the iSerialNumber string (index at 1824, read in `configure_storage`); init records the chosen disk's GUID and relaunches that driver with `driver_arg::EXPECTED_DISK` + a 47-bit digest (`sdk/abi/src/lib.rs` 68, init `set_driver_arg` 584 / 2069, `start_driver` 202); a restarted driver serves only the matching disk (2206; `expected_digest` 3350) and answers `Io` meanwhile (3220) |
| Minors | `scsi_failed` exempts `Transport(Refused)` (2257); `poll_hubs` stops for this poll when its time ran out instead of marking the hub failed (2431); strict halt on a timeout during unplug documented in ADR 0031; init probes all candidates with one `BlockClient` (`set_service`, `sdk/rust/src/block.rs` 43; init 2038) and logs a GPT read error separately (init 2047); `usb.ehci_companion` now expects the "reported" path and says QEMU reports N_CC = 0; QEMU fixture in `libs/ehci` uses the real 0x06 / 0x80 |

## Third audit (after `689d7dd`)

**Verdict:** one mandatory item left, plus recommended ones.

| Id | Finding |
|---|---|
| N1 (mandatory) | `break` after `serve_block` puts housekeeping between a request and the one queued behind it: 15 + 36 + 15 + 36 = 102 s > 90 s. Restore `continue` (the 8-message cap bounds the batch) and fix the comment |
| N2 | Every AriCodeOS image has the same GPT disk GUID (`[0xA5; 16]` in `libs/gpt`), so the restart identity cannot tell AriCodeOS sticks apart; the ADR must say so and that a restarted usbd ignores the expected-disk bits |
| Identity reads | A failed GPT header read or a transient serial read failure must fail the enumeration (retried on the next connect), not yield a different identity |
| Minor | A HID CLEAR_FEATURE refused for lack of housekeeping time must be retried later, not stop the HID until replug |

### Resolution

Verified with `cargo xtask check`, host unit tests and `cargo xtask build`;
QEMU not run.

| Id | Fix |
|---|---|
| N1 | `continue` restored after `serve_block` (`services/usb-ehci/src/main.rs` 3453); batch comment (3428) and budget comment at `REQUEST_BUDGET_MS` corrected: one housekeeping round plus two requests, 15 + 36 + 36 = 87 s |
| N2 | `libs/gpt`: `TEST_DISK_GUID` (176) and `write_disk_with_guid` (191, host test); `cargo xtask usb-image` writes a random version-4 disk GUID from `/dev/urandom` and prints it (`tools/xtask/src/image.rs` 416, 410); QEMU test images keep `TEST_DISK_GUID`. ADR 0031 says both, and that a restarted usbd ignores the expected-disk bits |
| Identity reads | A short GPT header read fails the enumeration (`main.rs` 2208; a failed read already did through `scsi_failed`); a serial read that fails other than by a STALL fails it too (2069) |
| Minor | A STALL's CLEAR_FEATURE is owed (`clear_owed`, 2563) and retried on the next poll when refused for lack of time (`clear_hid_halt` 2578); other failures still stop the HID |
