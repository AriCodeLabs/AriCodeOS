# ADR 0079: The panel's own size when the firmware does not offer it

- **Date:** 2026-11-05
- **Status:** Accepted. The owner chose a driver built from the manuals over any
  BIOS change, "because if Linux and other systems detect these devices, so
  should we", and moved it from the loader to the kernel.

## Context

ADR 0078 needs real pixels: its spacing is exact to the pixel. The reference
laptop (Ivy Bridge, HD Graphics 4000, LVDS panel) reported:

```
screen  800x600, the firmware's mode; the panel prefers 1366x768
modes   1024x768 800x600*
```

The EDID reads correctly, but the firmware's GOP offers two 4:3 modes on a 16:9
panel. Choosing among them cannot help.

Changing a BIOS setting might. It was rejected: it would help one machine, risk
a blank screen, and leave every other such machine as it is.

Microsoft's UEFI GOP test requires integrated displays to boot at their native
resolution, so firmware from the Windows 8 era on should offer it. We infer that
this mostly affects machines older than that. No survey confirms it.

## Sources

- **Intel's PRM for Ivy Bridge**, 2012: Volume 3 Part 2 (PCI registers) and
  Part 3 (North Display Engine): section 2 "Pipe timings change", 3.8
  watermarks, 3.4.4 ERR_INT, 4.1.7 SRCSZ, 4.5 panel fitter, 5.3 primary plane.
  The licence allows no derivative works of the document, so our code and
  documents describe it in our own words.
- **Linux i915** (MIT): `intel_pipe_fastset` and `ilk_pfit_disable`. Linux changes
  the source and removes the fitter on the running pipe on this family. The
  device IDs and the valid bit of a GTT entry, which the extract of the manual
  does not show, come from here.
- **Haiku's `intel_extreme`** (MIT), its tickets and comments: it relies on what
  the firmware left, and its developers say mode setting does not fully work on
  the HD 4000. So we read the state, and act only on what we read.

## What the hardware showed

A temporary read-only probe of 95 registers, all documented for this family:

- pipe A runs at the panel's timing, 1366x768 of 1466x788;
- its source is 800x600, and fitter 0 scales it to fill the panel;
- the primary plane is linear 32-bit BGRX at offset 0, stride 3200;
- the GTT maps 64 MiB of stolen memory, contiguous, from the aperture's start;
- the watermarks are at their defaults, the low-power ones off;
- no compression, one pipe, VGA plane off.

So no mode set is needed. Timings, clocks and the LVDS link stay as they are.

## Decision

1. **`libs/intel-display` decides, reading only.** It acts only on a family in
   its table (Ivy Bridge: 0156, 0166, 0152, 015A, 0162, 016A), with exactly one
   running pipe and every condition the method needs checked. The panel's size
   comes from the pipe's timing; an EDID, if the loader had one, must agree. Any
   other state keeps the firmware's mode.
2. **The change, in the order tried on the laptop:**
   - with the fitter on, the source and the stride become the panel's (the PRM
     allows this on the running pipe);
   - the next frame, the fitter leaves, as in Linux. The PRM lists this step only
     with the pipe off; on this family Linux does it live, and it worked on the
     laptop with no visible change. The source now equals the active size, as
     the PRM requires without a fitter;
   - a whole frame later, the pipe's FIFO underrun and plane fault bits must be
     clear. Any failure restores what was there.
3. **In the kernel, not the loader.** It was first written in AriBoot, which
   needs no new authority either. But AriBoot has one copy and no network update:
   a defect there has no rollback and needs a new stick, and installed machines
   would never get it. The kernel is in the A/B slots and updates over the
   network, so this is what the owner chose. It runs after PCI enumeration, so it
   uses the BARs ADR 0051 checked, and before any program has the screen.
4. **No new authority for any program.** Programs see only a larger screen. BAR 0
   stays mapped in the kernel only.
5. **The way back.** `video=firmware` in `cmdline.txt`, from any other computer,
   touches nothing. Any `video=` leaves the choice to the loader.
6. **`diag` tells.**
   ```
   screen  1366x768, the panel's own, set on the running pipe; the panel prefers 1366x768
   ```
   or why not: `; not its own size: the pipe is not at the panel's size`. Without
   an EDID and without support, it says nothing: no fault is known then.

## The resources this device offers

| Resource | Today | Next |
|---|---|---|
| Panel's native size | Used | |
| Write-combining | Used: 43.6 ms to 1.33 ms, full flip at 1366x768 | |
| Panel fitter | Off: the plane's data bypasses it | |
| Hardware cursor plane | Not used | The pointer without redrawing; needs a runtime driver |
| Flip at vertical blank | Not used | No tearing; needs a runtime driver |
| Frame buffer compression | Not used | Fewer memory reads; to be measured |
| Backlight (BLC_PWM) | Not used | Brightness from settings |
| Sprite planes | Not used | Layers without copying |

Those need a driver running while the system does, with its own capability. That
is a later decision. This one is a single step at boot.

The compositor now maps only what the screen takes, not the firmware's 64 MiB
region, and the kernel's physical map keeps 2 MiB pages inside it.

## Consequences

- **Boot:** the first lines appear at the firmware's mode, then the console
  starts over on the larger screen. Those lines stay on serial.
- **Older sticks** get this by `upgrade`, since the kernel needs nothing new from
  the loader.
- **A firmware that reports a framebuffer region smaller than the new screen** is
  refused (`TooLarge`): the physical map covers only that region. To be lifted
  when a machine shows it.
- **Low-power watermarks on, or the pipe's below its default,** are refused: we
  have no Programming Watermarks document to compute them from. On the reference
  laptop they are off.
- **Other families** (Sandy Bridge, Haswell and on) stay as the firmware left
  them. Each enters with its manual and a machine to test on (skill
  `aricodeos-device-study`).
- **Tests:** host tests use a register bank with the manual's double buffering and
  the state read from the laptop, including every refusal and failures at each
  frame. QEMU does not emulate this hardware, so only the laptop covers the real
  change. The manual does not say whether reads return armed or live values, so
  the read-back may only prove that a write landed; the underrun check is the one
  that proves the pipe is healthy.
- **Not tested on hardware:** the refusals and the restore path. They would need
  a machine in another state.

## Audit

An independent read-only review found no high findings. All fixed:

- **M1.** The console repainted the whole screen with interrupts off, against
  incident 0031. It now starts over and paints in steps.
- **M2 (suspected).** Low-power watermarks computed for 800 pixels could underrun
  at 1366. Those states are refused, and the underrun bits are checked after the
  change.
- **L3.** After a failed change the screen stayed blank. The console repaints.
- **L4.** A 3x3 fitter with a source over 2048 pixels is refused, per the PRM.
- **L5.** Compression, a sprite or asynchronous flips on are refused.
- **L6.** `diag` said "no usable EDID" under `video=firmware`, and marked the old
  mode as chosen. Fixed.
- **L7.** A refused BAR reported "no support". It now reports the BARs.
- **L8.** Pages past the memory limit and outside the range were mapped.
- **Tests:** the fake bank applied writes at once, so it could not catch an arming
  bug, and it only ran pipe A. It now latches at vertical blank, and covers every
  pipe, more refusals, a freeze at each frame and the restore order.
