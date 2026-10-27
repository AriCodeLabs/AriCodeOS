# Incident 0031: the screen console repaints so slowly that kernel image layout decides whether tests pass

- Opened: 2026-10-25. Status: root cause found; fixed on `fix/console-repaint`.
- Found by: the full suite with 1 CPU on `fix/pci-device-bounds`
  (`0a69467`): `usb.hid_fault` timed out; the same suite with 4 CPUs
  passed, and so did the previous commit of that branch.
- Environment: QEMU TCG (every scenario), 1 vCPU; on real hardware the
  same code is slow for its own reasons (a full repaint per line).

## Evidence

Deterministic on `0a69467`: `cargo xtask test --all --area usb,input,diag`
failed `usb.hid_fault` 8 of 8 times at the same step: `diag` ran, its
prompt came after the runner's 10 s. Measured with `uptime` around
commands: `diag` took 12 s instead of 5 s; `usb.hid_fault` and
`strict.usb_hid_halt` took 35-50 % longer; nothing else changed. The kernel
logs of fast and slow builds are identical apart from the binary's size.

Bisecting the branch's commit pointed at code that changes no decision
and touches no device; one leaked heap allocation flipped a fast build to
slow. The study (instrumented guest, a RIP sampler on QEMU's vCPU thread
with debug symbols, gdb on QEMU's victim-TLB path) found:

- during `diag` about 98 % of guest time draws the screen console: each
  newline at the bottom repaints the whole 1280×800 screen, about one
  million `Surface::put` calls, with interrupts off (40 ms per scroll
  normally, 75-85 ms in the slow builds);
- timer ticks are lost meanwhile (95 in 5.4 s instead of 540): a 500 ms
  sleep took 5.4 s;
- slow runs spend 45-55 % of the vCPU in QEMU's memory slow path
  (`victim_tlb_hit`, `mmu_lookup`), fast runs 0-3 %: 3909 of 4000 victim
  hits alternated between the tail page of `console::FB` (where `fg`/`bg`
  live) and the top page of the shell's kernel stack, which share a slot
  of QEMU's direct-mapped software TLB (the kernel image, the stack region
  and the physical map are all 2 GiB aligned, so pages at the same offset
  collide at every TLB size).

Which builds are slow depends on where `console::FB` lands in `.data`
against the shell's kernel stack slot (`tid × 0x9000 + 0x8000`): a code
change that moves `.text` across a page boundary moves `.data` by a page.
develop has the same latent defect: 12 KiB of padding alone made it fail
the same way.

## Mechanism

`aricode_fb_console::Console::draw_glyph` calls `Surface::put` once per
pixel; each call reloads the colours from the console structure and goes
through the surface's bounds check and colour encoding, spilling to the
stack; `scroll_up` repaints every cell on every newline; all of it runs
inside the `IrqLock` of `console::FB`. Each pixel touches several pages,
and when two of them alias in QEMU's TLB every pixel costs two victim-TLB
swaps. Scenarios that type and wait for echoes within fixed windows (10 s)
then fail on layout luck.

## Fix

On `fix/console-repaint`, three changes to the console and the re-timing
of the scenarios that leaned on its slowness.

### Drawing: a glyph per surface call

`Surface::glyph(x, y, rows, fg, bg)` draws one 8x16 cell. The default
goes through `put` (host tests); `MmioSurface` encodes both colours once,
checks the bounds once per pixel row (width, height and the
framebuffer's size) and writes the eight pixels in a tight loop of
volatile stores; `fill_rows` does the same for the clear. The console
calls nothing else to draw. Per pixel the work is now a store, so two
aliasing pages cost a TLB swap per glyph, not two per pixel.

### Repainting: a RAM shadow, only what changed, once per message

Options weighed:

- *Pixel back buffer in RAM, blitted after a scroll.* Needs 4-8 MiB
  (heap, not the static console), and every scroll still writes every
  pixel of the screen to the framebuffer: on real hardware, VRAM writes
  over PCIe dominate, so it does not get cheaper than the old repaint.
- *Scroll by reading the framebuffer back* (`memmove` in VRAM): reads
  of VRAM are uncached and very slow on real hardware. Ruled out.
- *Jump scroll* (move several lines at once): fewer repaints but a
  different behaviour on screen, and still a full repaint each time.
- *Shadow of the drawn cells* (chosen, the approach of Linux fbcon's
  redraw mode): the console keeps `drawn`, what the screen shows in each
  cell (32 KiB more next to `cells`, no heap, the console stays `const`
  in a `static`), and a range of dirty rows. Writes only change `cells`
  and mark rows; `redraw(max_rows)` paints the cells where `cells` and
  `drawn` differ, at most `max_rows` rows per call. Blank areas never
  change, a line equal to the one it replaces costs nothing, and nothing
  is ever read from the framebuffer. Shell output shifted by a line still
  differs in most non-blank cells, so the second half matters as much:
  a message is painted once at its end however many lines it scrolls
  (`write_str`, `write_fmt`, and the kernel's `write_raw`). Cursor cells
  and a returned screen are recorded as unknown (`drawn` 0, never a
  character) so the next redraw restores them; `set_colors` invalidates
  the shadow.

### Locking: never off for a whole repaint

`write_raw` writes serial under its own lock first (the screen no longer
holds other writers' serial output up). Then, with interrupts enabled
(thread code), it waits for the console lock, queues the message into the
cells and paints one text row per hold, releasing the lock, and with it
`cli`, between rows: at most one row of glyphs (160 cells, about 20 000
stores at 1280 px) runs with interrupts off. Whoever holds the lock next
paints whatever is still dirty, so a writer preempted between rows, or
another CPU's writer, leaves nothing inconsistent; waiting instead of
`try_lock` also stops other CPUs' lines from being dropped from the
screen when the lock was busy. The holder always has interrupts off, so
a waiter with interrupts enabled is never waiting on its own CPU. With
interrupts off (handlers, exceptions) the console only retries for a
bounded time, as `serial_guard` does, and paints in one hold (releasing
would not let the tick in); if the lock stays held (an exception inside
the console) the text goes to serial only, as before. The panic handler
still writes serial only and is unchanged.

### Measured

`cargo xtask test --all --area usb,input,diag` (46 scenarios, 10 at a
time), elapsed in seconds; `PAD` is `#[used] static PAD: [u8; N] = [1;
N];` added to `kernel/src/main.rs` to move `.data`, then removed:

| build | N | diag.events | usb.hid_fault | strict.usb_hid_halt |
|---|---|---|---|---|
| develop `df15084` | 0 | 19.7 | 18.8 | 20.8 |
| develop `df15084` | 4096 | 21.3 | 19.1 | 21.7 |
| develop `df15084` | 12288 | 22.6 | 60.0, timeout | 35.0 |
| fix | 0 | 8.4 | 8.4 | 10.2 |
| fix | 4096 | 8.6 | 8.2 | 11.0 |
| fix | 12288 | 9.8 | 9.1 | 10.2 |

`strict.usb_hid_halt` now includes an explicit 3 s `sleep` (below). The
full suite runs in 3m00s of wall clock with 1 CPU and 5m01s with 4
(192 scenarios each, 0 failed), against about 4.5 and 7.5 minutes before.

### Scenarios that depended on slow console output

With drawing fast, 16 scenarios failed on the first full run. Each was
fixed on its own merits; none by slowing the console or by retries.

- **13 USB storage fault scenarios** (`usb.fault_retry`, `usb.fault_csw`,
  `usb.short_csw`, `usb.short_data`, `usb.reenum`, `usb.reset_device`,
  `usb.unreleased_slot`, `usb.ehci_fault`, `usb.ehci_fault_csw`,
  `usb.ehci_short_csw`, `usb.ehci_short_data`, `strict.usb_halt`,
  `strict.ehci_halt`): `usb_fault=idle...` injects on the first block
  request after two idle seconds. The scenarios made those seconds with
  `repeat 10 ping 10.0.2.2`, 50 lines of output that took 2-4 s only
  because each line cost a full repaint; now they take milliseconds, the
  `write` came too early and no fault was injected. They now use
  `sleep 3000` (the shell command added for incident 0027, later than
  these scenarios): an explicit wait in guest time.
- **`strict.usb_hid_halt`**: the host types a key over QMP on
  `CONSOLE_BRIDGE_READY` (the runner pauses 0.2 s before typing) and the
  injected HID failure halts usbd; `diag` must come after. The same pings
  kept the shell busy "until the host has typed the key"; now `sleep
  3000`, an order of magnitude above what the key and the halt need.
- **`usb.hid_fault`, `shell.usb_keyboard`** ("failed after 29 events,
  Broken pipe"): a runner race. Each key went out as two QMP commands,
  press then release; the Enter press runs `shutdown`, and the guest now
  powers off before the release command. The runner now sends a whole key
  (modifiers, press and release) in one `input-send-event`: QEMU queues
  all of it in the device before answering, the guest still sees press
  before release, and the pause after a key is unchanged (`key_delay` per
  event). This also removes the lost QMP reply of incident 0019.
- **`console.ps2`, `console.ps2_es`** (after that runner change: 11
  scancodes instead of 10): `test=console_ps2` reported the i8042 bytes
  read *when the line was printed*, so the Enter release was counted or
  not depending on timing (the 10 were the keyboard's 0xFA acknowledgement
  of its set-up plus every byte up to the Enter press, not the release as
  the scenario's comment said). The kernel now records the count when the
  Enter press is decoded; the expected 10 and 18 are unchanged and no
  longer racy.

## Audit (2026-10-25)

An independent read-only audit of the branch found no High findings.
Follow-ups, one commit:

- **M1**: `blink` ran `redraw(usize::MAX)` under the cursor thread's
  `try_lock` with interrupts off. After a screen came back from a driver
  under test (every cell dirty) or with a writer preempted between rows,
  the cursor thread could paint the rest of the screen in one hold: the
  incident's 40-85 ms with interrupts off. `blink` now draws one glyph at
  most (the cursor, or the cell it hid) and does nothing while rows are
  pending; the cursor thread paints pending rows in steps
  (`paint_in_steps`) before blinking, so they are never left behind.
- **M2**: thread writers used `IrqLock::lock`, which panics after 2^26
  spins; before the fix `write_raw` never panicked (it only used
  `try_lock`). The console now waits with its own loop (`wait_fb`:
  `try_lock` and `spin_loop`, interrupts restored between attempts, no
  counter): every holder has interrupts off, so it is on another CPU,
  holds the lock for one row and waits on nothing.
- **L1**: a writer with interrupts off painted everyone's backlog in one
  hold. It now paints one row (a line that did not scroll is all there
  is) and leaves the rest to the next thread writer or the cursor thread.
  Every hold of `FB` after boot is now one row or one glyph.
- **L2**: serial was released before the screen was taken, so two writers
  could come out A, B on serial and B, A on screen (on a laptop without a
  serial port the screen is the log). The message is now queued for the
  screen while the serial lock is still held (order `SERIAL` then `FB`,
  `FB` a leaf); only the painting runs without it.
- **L3**: `test=console_ps2` read `PS2.lock()` inside the `println!`
  arguments, holding the guard for the whole console write; read into a
  local first.
- **L4**: lines logged while the screen was lent were dropped, although
  `reattach` says they are kept. They are now queued while detached (a
  redraw without a screen drops only the dirty range; `reattach`
  repaints every cell). The console ignores writes before its first
  `attach` (no grid yet).
- **I1**: comments updated: the console static holds two arrays, about
  64 KiB.
- **I3** (optional, not done): counting the screen lines dropped when
  the bounded retry gives up needs a diagnostics path for kernel console
  counters that does not exist yet; such lines still reach serial.

New host tests: `blink_draws_at_most_one_glyph` (pending rows after a
reattach or a deferred write: no painting; then one glyph to show and
one to hide); the detach test now also checks the whole screen against
the cells after the reattach. `cargo xtask test --all --area
console,usb,input,diag,drivers,shell`: 122 scenarios, 0 failed (2m19s);
host tests 650 passed.

## Open

- `cargo xtask perf` baselines were not re-measured on this branch: the
  console is much faster, so baselines that include console output may
  move (they are taken before `main` anyway).
- Kernel image grows by 32 KiB (`drawn` in the console's `static`,
  `.data`).
- I3 of the audit (a counter of screen lines dropped by interrupt-context
  writers) waits for a diagnostics path for kernel console counters.
