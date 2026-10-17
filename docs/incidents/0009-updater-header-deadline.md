# Incident 0009: response headers bypass the updater run deadline

- Opened: 2026-10-18. Status: **fixed in `fix/updater-absolute-deadline`,
  pending QEMU run and audit** (2026-10-18).
- Reviewed commit: `525b550`; the run deadline is new in the update client.
- Environment: production HTTP parser with a transport supplying 1xx replies.
- Evidence: [release audit, finding 3](../audits/2026-10-18-release-0.11.0-525b550.md#3-p2-the-updaters-global-deadline-does-not-bound-response-headers),
  [probe output](../audits/525b550-review/probes.log),
  [probe source](../audits/525b550-review/http.rs.in).

## Observation

The transport supplied 700 `103 Early Hints` replies, advancing the clock
one second per receive, then a zero-body 200 reply. The parser returned
success after 701 simulated seconds; the sink was never called despite
the updater's 600-second run budget:

```text
HTTP: Ok(200), elapsed=701000ms, deadline=600000ms, sink calls=0
```

Real-time network reproduction was not performed; indefinite delay follows
from the unbounded interim-response loop.

## Confirmed mechanism

The updater's ten-minute deadline (`RUN_DEADLINE_MS`, `remaining_ms()`)
was enforced in two places only: once before each download and inside the
body sink. The transport received a fixed per-operation timeout
(`left.min(HTTP_TIMEOUT_MS)`, computed once) for the name resolution, the
connection, the TLS handshake and every send and receive.

`aricode_http::fetch_with` loops over interim (1xx) responses and header
bytes without calling the sink, and nothing counted them: a server that
sends a small interim response, or one header byte, before each
per-operation timeout keeps the parser progressing forever. The chunk
decoder had the same gap for chunk framing and trailers, and the TLS
stream for records that carry no application data. All of this happens
before any signature check, so it needs no key.

## Fix

Branch `fix/updater-absolute-deadline` (ADR 0033 records the contract):

- `libs/http`: `fetch` and `fetch_with` take a `&dyn Budget` ("milliseconds
  left for the whole fetch") and ask it before every send and every
  receive, in the request, headers, body, chunk-framing and trailer
  phases; `0` fails with `Error::Deadline(Phase)`. A transport failure once
  the budget is spent is reported as the deadline too (the transport was
  given at most the time that was left). Defence in depth: at most
  `MAX_INFORMATIONAL` (8) interim responses per fetch
  (`Error::TooManyInformational`) and at most `MAX_HEADER_BYTES` (16 KiB)
  of header bytes per fetch, discarded 1xx heads included
  (`Error::HeaderBytes`). `MAX_HEADERS` (8 KiB per response) is unchanged.
- `sdk/https`: `get` and `get_with` take the budget next to the
  per-operation `timeout_ms`; every resolve, connect, TLS record exchange,
  send and receive gets `min(remaining, timeout_ms)`, and the handshake
  loop checks the budget between records (`HttpsError::Deadline(Phase)`,
  phases `Resolve`, `Connect`, `Handshake`, `Request`, `Headers`, `Body`).
  Sends are bounded too (`NetClient::tcp_send_all_within`).
- `sdk/https`, after the audit of the first fix commit (below): the
  receive loop after the handshake also asks the budget between records
  and refuses more than `MAX_SILENT_BYTES` (64 KiB) of records in a row
  that carry no application data (`HttpsError::EmptyRecords`).
- `sdk/rust` `http::get` takes the budget as well; the test client, which
  has no clock handle and talks to the host's test server only, passes
  `Unbounded` explicitly.
- Updater: `fetch` passes `run_budget` (what is left until `DEADLINE`) and
  maps the new errors to messages; `download_fault` turns them into the
  existing `upd-deadline` fault (code 51), e.g. `source 1 http://…: cannot
  fetch the index: deadline passed while waiting for the response headers
  (run deadline)`. The sink no longer checks the clock: the fetch does.
- Shell `http`: 8 s per operation, 30 s for the whole command; new
  messages `http: deadline passed while …`, `http: too many interim
  responses`, `http: response headers too large in all`.

## Audit of the fix (2026-10-18, commit 0527af9)

An independent review of the first commit found one gap of the same
class one layer lower: the TLS branch of `Stream::recv` looped over
`pump()` until plaintext appeared without consulting the budget. `pump()`
bounds each `tcp_recv` by `min(remaining, timeout_ms)`, but a server that
keeps bytes queued never reaches that timeout, and records without
application data are accepted silently: zero-length application-data
records (RFC 8446 §5.1; `libs/tls/src/record.rs` accepts an empty
plaintext, `libs/tls/src/lib.rs` pushes no event for it) and
post-handshake `NewSessionTicket`. Once the budget was spent the timeout
was `0`, which netd clamps to 1 ms but answers at once when data is queued,
so a hostile source streaming empty records at one per 10 ms would have
held `update` for ever after the handshake. The first commit's wording
("every TLS record exchange bounded") was therefore overstated for that
loop; corrected above and in ADR 0033.

Second commit: the loop returns an error when the budget is spent
(`fetch_with` names the phase) and counts the record bytes received since
the last byte of plaintext, refusing more than `MAX_SILENT_BYTES` (64 KiB,
in bytes rather than records so a large record arriving in small segments
is not mistaken for a stall) with `EmptyRecords`. The TCP and TLS layers
of the stream are behind the private `Tcp` and `Records` traits so the
loop is tested on the host. Low findings fixed in the same commit: the
`Slow` mock already refused to wait past the deadline, so the first
tests proved the error mapping rather than the parser ending a stall
(`parser_alone_ends_a_stall_on_a_transport_that_never_times_out` uses a
transport with a fixed per-operation timeout, the pre-fix situation, and a
runaway cap); `aricode_sys::http::get` now reports a netd timeout at the
deadline as `Deadline(Resolve | Connect)` like `aricode-https`; the
cumulative header-byte check refuses at `> MAX_HEADER_BYTES`, documented.

## Second audit (2026-10-18, commit 76b88d3)

Nothing serious. Confirmed: the silence counter's semantics (record bytes
since the last byte of plaintext, reset by data, applied after the
handshake only), the `Records` trait as a one-to-one delegation to
`aricode_tls::Client`, and mutation runs (removing the budget checks makes
the tests fail). Two small items, fixed in the final commit: the `>`
boundary of `MAX_HEADER_BYTES` was not pinned (mutating it back to `>=`
passed every test), so `cumulative_header_bytes_boundary` accepts exactly
the limit and refuses one byte more, and the constant's comment states
that the bound is soft by what the completing receive brings (at most
`MAX_HEADERS` minus the final head already held, about 24 KiB in all);
and `parser_alone_ends_a_stall_on_a_transport_that_never_times_out` says in
its comment that it documents behaviour 0527af9 already had.

## Tests

Host (`cargo test -p aricode-http`, simulated clock over the mock
transport, like the audit's probe):

- `interim_response_flood_is_refused_long_before_the_deadline`: the audit's
  exact probe (700 × 103, one second each) now fails with
  `TooManyInformational` after 9 simulated seconds, sink never called.
- `slow_interim_responses_fail_at_the_deadline`: interim responses every
  100 s (so the count limit cannot fire first) end with
  `Deadline(Headers)` at exactly 600 s, sink never called.
- `header_dribble_fails_at_the_deadline`: one header byte per receive.
- `endless_chunk_trailers_fail_at_the_deadline`: chunk framing without a
  body byte (`Deadline(Body)`); a never-ending chunk size line is refused
  by the existing line limit.
- `transport_timeout_past_the_deadline_is_the_deadline`, and a transport
  error with time left stays a transport error.
- `too_many_informational_responses`, `cumulative_header_bytes_are_bounded`
  (exactly the limits are accepted, one more is refused; large legitimate
  headers still fit).
- `fast_fetch_within_budget_succeeds`: a normal fetch with one interim
  response and a body over several timed receives is untouched.

- `parser_alone_ends_a_stall_on_a_transport_that_never_times_out`: with a
  transport that always delivers (fixed per-operation timeout), the budget
  check before each receive ends a header dribble at the deadline after
  exactly ten receives.

`cargo test -p aricode-https`: the per-operation timeout follows the
budget (`operation_timeout_follows_the_budget`), failures before the HTTP
exchange become the deadline only once the budget is spent, and over a
simulated TLS stream with data always queued:
`empty_records_forever_end_at_the_deadline` (one empty record per second,
`Deadline(Headers)` at exactly 60 s, sink never called),
`empty_records_in_bulk_are_refused_by_the_silence_limit` (`EmptyRecords`
after 64 KiB, under a second), `handshake_without_progress_ends_at_the_deadline`
(`Deadline(Handshake)`) and `plaintext_within_budget_is_delivered`.

QEMU, written but **not yet run** (another process owned the QEMU ports
when the fix was made): `shell.http_deadline`
(`tests/qemu/shell_http_deadline.toml`) against two new routes of the
xtask test HTTP server: `/early-hints` (a 103 every five seconds, never a
final response: the shell's 30 s budget cuts it with `deadline passed
while waiting for the response headers`) and `/early-hints-flood` (twenty
103 at once: `too many interim responses`).

## Open

- Run `shell.http_deadline` and the existing network, update and HTTPS
  scenarios (`cargo xtask test --all`), then the failure-protocol audit of
  the fix against this incident.
- A real-time reproduction against an adversarial server over the network
  has not been performed; the simulated clock is the evidence.
