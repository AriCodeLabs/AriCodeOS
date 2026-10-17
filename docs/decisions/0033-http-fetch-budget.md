# ADR 0033: An absolute budget for every HTTP fetch

- **Date:** 2026-10-18
- **Status:** Accepted (incident 0009; amends ADR 0026 and ADR 0029)

## Context

The update client has a ten-minute deadline per run (ADR 0029, audit 1
M3), checked before each download and inside the body sink. The release
audit of 0.11.0 (finding 3, incident 0009) showed that a source can hold
`update` indefinitely without delivering a body byte: the HTTP client
consumed interim (`1xx`) responses, header bytes, chunk framing and
trailers with a fixed per-operation timeout and no absolute bound, and the
sink, where the deadline lived, was never called. The same held for the
TLS handshake and for TLS records without application data. All of it
happens before any signature check.

## Decision

1. **A fetch carries a budget.** `aricode_http::Budget` answers
   `remaining_ms()`, the milliseconds left for the whole exchange (any
   `Fn() -> u32` is one; `Unbounded` is the explicit opt-out for a caller
   without a clock talking to a server it controls). `fetch` and
   `fetch_with` take a `&dyn Budget` and ask it before every send and every
   receive, in every phase; `0` fails with `Error::Deadline(Phase)`
   (`Request`, `Headers`, `Body`). A transport failure once the budget is
   spent is reported as the deadline: the transport was given at most the
   time that was left.
2. **Transports derive each operation's timeout from it.** `aricode-https`
   `get`/`get_with` and `aricode-sys` `http::get` take the budget next to
   the per-operation `timeout_ms`; every resolve, connect, TLS record
   exchange, send and receive waits `min(remaining, timeout_ms)`, and the
   pre-HTTP phases report `Deadline(Resolve | Connect | Handshake)`. A
   timeout alone does not bound a TLS stream whose server keeps bytes
   queued: records without application data (zero-length records, RFC
   8446 §5.1, session tickets, key updates) make a receive return at once
   with no plaintext. So both TLS loops, the handshake and the receive
   loop after it, ask the budget between records, and the receive loop
   refuses more than `MAX_SILENT_BYTES` (64 KiB, counted in bytes so a
   large record arriving in small segments is not a stall) of such
   records in a row (`EmptyRecords`). Sends are bounded as well
   (`NetClient::tcp_send_all_within`; `tcp_send_all` keeps its five-second
   idle limit).
3. **Defence in depth in the parser.** At most `MAX_INFORMATIONAL` (8)
   interim responses per fetch (`TooManyInformational`) and at most
   `MAX_HEADER_BYTES` (16 KiB) of header bytes per fetch, discarded `1xx`
   heads included (`HeaderBytes`); `MAX_HEADERS` (8 KiB per response) is
   unchanged. These bound the work, the budget bounds the time; neither
   replaces the other.
4. **Callers.** The updater passes what is left of its run (`run_budget`)
   and reports the new errors through the existing `upd-deadline` fault
   (code 51): `deadline passed while waiting for the response headers (run
   deadline)`. The shell's `http` gets 8 s per operation and 30 s in all.
   The test HTTP client (`user/testprogs`, no `SYSTEM` handle) passes
   `Unbounded`.

## Consequences

- Public API change in `aricode-http`, `aricode-https` and
  `aricode-sys::http`: one more parameter; the error enums gain
  `Deadline(Phase)`, `TooManyInformational` and `HeaderBytes`. No caller
  can forget the budget: there is no default.
- Host tests (`aricode-http`): the audit's probe (700 × `103`), slow
  interim responses, a header dribble, endless chunk trailers, a transport
  timeout at the deadline, both limits at their edges, a normal fetch
  under budget, and the parser alone ending a stall on a transport that
  never times out. `aricode-https` (the TCP and TLS layers of the stream
  are behind private `Tcp` and `Records` traits so it runs on the host):
  the per-operation timeout derivation, empty TLS records for ever ending
  at the deadline, the silence limit, a handshake without progress, and
  plaintext delivered.
- The error enums also gain `EmptyRecords` (`aricode-https`).
- QEMU scenario `shell.http_deadline` with the xtask routes
  `/early-hints` (one `103` every five seconds, never a final response) and
  `/early-hints-flood` (twenty at once); written with the fix, to be run
  before the incident closes.
- Open: a trusted server that legitimately needs more than eight interim
  responses or 16 KiB of headers is refused; the limits are constants in
  `libs/http` and can be raised with a test.
