# Incident 0007: EHCI accepts a short command-status wrapper

- Opened: 2026-10-18. Status: open, reproduced on the host.
- Reviewed commit: `525b550`; introduced with the EHCI driver in 0.11.0.
- Environment: extracted production `Ehci::scsi`, simulated bulk transport.
- Evidence: [release audit, finding 1](../audits/2026-10-18-release-0.11.0-525b550.md#1-p1-ehci-acknowledges-a-write-without-receiving-a-complete-csw),
  [probe output](../audits/525b550-review/probes.log).

On a WRITE(10), the mock completed the data phase and transferred only the
CSW signature and current tag (8 bytes). The existing residue/status bytes
were zero. `scsi` returned `Ok(0)` and performed no Reset Recovery.

Confirmed mechanism: `services/usb-ehci/src/main.rs:2684` ignores the
successful bulk transfer's byte count, then parses a full 13-byte buffer.
The recovery CSW read has the same omission. A short reply can therefore
be acknowledged using stale status bytes.

No production fix has been applied. Next: independent failure-protocol
reviews, enforce exact CSW length in both paths, retain a regression,
verify strict-mode behaviour and rerun USB/QEMU coverage. Physical device
reproduction and actual data-loss measurement remain unperformed.
