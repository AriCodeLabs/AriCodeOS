# Incident 0008: installer loses state after a directory-list error

- Opened: 2026-10-18. Status: open, reproduced on the host.
- Reviewed commit: `525b550`; concerns the new shared updater state.
- Environment: extracted installer helpers with one failed list request.
- Evidence: [release audit, finding 2](../audits/2026-10-18-release-0.11.0-525b550.md#2-p2-a-transient-directory-list-error-can-erase-updater-state),
  [probe output](../audits/525b550-review/probes.log).

An existing `updater/state` contained installed, plan and staged sections.
Its first directory lookup failed; the following open/replace succeeded.
Writing the installed section returned success and removed both other
sections without raising `inst-state-unreadable`.

Confirmed mechanism: `file_size` conflates errors with absence;
`read_state` returns empty state on either outcome. The subsequent atomic
replace faithfully persists the incorrectly empty starting state.

No production fix has been applied. Next: independent failure-protocol
reviews, propagate lookup errors, preserve state on every unsuccessful
read, add the regression and review installed-version callers of the same
helper. No physical disk failure was induced in this audit.
