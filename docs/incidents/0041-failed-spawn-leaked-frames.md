# Incident 0041: a spawn that failed half-way kept the frames it had taken

- Opened: 2026-10-25. Status: fixed on `fix/kernel-error-paths`.
- Found by: the failure-pattern audit of 2026-10-25 (audit C, L1-L3:
  error paths that skip the cleanup the main path does). Not seen on a
  machine.
- Environment: any boot under memory pressure; a supervisor relaunching a
  program that fails to spawn loses memory on every attempt.

## Evidence

`test=spawn_oom` (scenario `user.spawn_oom`) ported onto develop at
`e2f6192`, without the fix:

- with all but 4 frames above the kernel reserve held, the spawns did not
  fail: the image and stack frames were taken from the reserve
  (`spawn must run out of frames`);
- with only 4 frames left in all, the 10 spawns failed and kept what they
  had mapped: `frames lost by 10 failed spawns: 4`.

## Mechanism

`spawn` built the address space step by step (root table, image pages,
stack) and returned the error of the step that failed; the page-table
walker has no destructor, so the root, the tables and the pages mapped so
far stayed allocated. The image and stack frames came straight from the
PMM, below the reserve ADR 0049 keeps for the kernel's own allocations.
Next to it, a child the parent's handle table could not hold was left
running unowned, and `pci_claim` could insert some of a device's handles
and fail on the rest.

## Fix

See `fix/kernel-error-paths`: a failed spawn frees its whole address
space (`free_address_space`, shared with reap); segment and stack frames
respect the reserve (`user_frame`); a child the parent cannot hold is
killed; `pci_claim` checks the table has room first and takes its handles
back if the copy-out fails; the IOMMU `map_identity` maps all or nothing.
