# Incident 0009: response headers bypass the updater run deadline

- Opened: 2026-10-18. Status: open, reproduced with a simulated clock.
- Reviewed commit: `525b550`; the run deadline is new in the update client.
- Environment: production HTTP parser with a transport supplying 1xx replies.
- Evidence: [release audit, finding 3](../audits/2026-10-18-release-0.11.0-525b550.md#3-p2-the-updaters-global-deadline-does-not-bound-response-headers),
  [probe output](../audits/525b550-review/probes.log).

The transport supplied 700 `103 Early Hints` replies, advancing the clock
one second per receive, then a zero-body 200 reply. The parser returned
success after 701 simulated seconds; the sink was never called despite
the updater's 600-second run budget.

Confirmed mechanism: the absolute deadline is checked before the call and
in the body sink, while the transport receives retain a per-operation
timeout. Interim responses keep the parser progressing without entering
the sink or exhausting a cumulative header limit.

No production fix has been applied. Next: independent failure-protocol
reviews, carry the absolute deadline into every network operation, and
test headers, framing, trailers and TLS progress. Real-time network
reproduction remains unperformed; indefinite delay follows from the
unbounded interim-response loop.
