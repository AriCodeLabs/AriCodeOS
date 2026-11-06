# Incident 0059: A scenario passed on a stage that never ran

- Opened: 2026-11-06. Status: fixed on `feature/window-protocol-6`.
- Found by: the independent audit of the window protocol, reading the log of a
  run the suite had reported as a pass.

## Evidence

`gui.window_events` passed (26.8 s, in `~/aricode-logs/test-gui-proto.log`), and
I told the owner so. Its log ended with:

```
[host] QMP: marker 'gui_demo D: leave' never seen; nothing sent
```

The scenario's last stage never happened: the compositor did not send `Leave`
over a window's frame, so the program was never told to finish. Yet its
`serial_order` requires `gui_demo D: leave`, and the check passed.

## Cause

The runner appends what it did to the capture the checks read, and that line
quotes the marker. So the text a scenario expects was found in the runner's own
statement that the text never came. Two defects together:
1. A stage whose marker never came was not a failure.
2. The checks read the runner's lines along with the guest's.

## Fix

Any `[host] QMP: marker` or `[host] flood: marker` line is now a failure of the
scenario. It is reported with the line itself. `usb.ehci_hotplug` keeps its own
rule for hotplug, which it checks by name.

## Reach

Every scenario with `[host_input]` could have hidden this, so the full suite ran
after the fix, at 1 and 4 CPUs. See the commit's log for its result.

## Lesson

The checks and the runner share one text. Anything the runner writes must not be
able to satisfy an expectation; and a pass is only as good as what it can fail on.
The scenario also had a weak assertion, which the same audit listed and which is
now strict: `key repeats 0\n`, a text that never appeared whichever way it went.
