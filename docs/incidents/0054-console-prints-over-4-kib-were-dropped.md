# Incident 0054: console prints over 4 KiB were dropped

- Opened: 2026-10-31. Status: fixed on `feature/adopt-rustls`.
- Found by: the work on ADR 0067. `net.https` stopped finding
  `s_server -www` in the body the shell's `http` prints. The download
  itself succeeded (`HTTP 200, 4197 bytes`).

## Evidence

`build/scenarios/net.https/serial.log`:
- `HTTP 200, 4197 bytes` is followed directly by the next prompt, with no
  body;
- the TLS 1.2 fetch shows the same: `HTTP 200, 4212 bytes`, no body.

`openssl s_server -www` lists the cipher suites both sides share. rustls
offers more of them than the old client did, so the page grew past 4 KiB.

## Mechanism

The kernel's `console_write` refuses a write longer than 4096 bytes with
`Error::Invalid` (`kernel/src/user/syscalls.rs`). `aricode_sys::print`
ignored the result. So any print longer than 4 KiB vanished without a
trace, from any program.

The shell keeps up to 8 KiB of an `http` body, so it could never show a
body between 4 and 8 KiB. Before ADR 0067 none of the test pages reached
that size.

## Fix

`print` writes its text in pieces of at most `CONSOLE_WRITE_MAX` bytes.
Each piece ends between UTF-8 characters, because the kernel takes only
valid UTF-8 in each write. The limit moved to `aricode-abi`, and the
kernel uses the same constant.

A host test (`console_pieces_respect_the_limit_and_utf8`) checks the
pieces:
- they fit the limit;
- a three-byte character across it moves whole to the next piece;
- text that is not UTF-8 is still cut at the limit, with nothing lost.

`net.https` shows both bodies again.

## Not changed

A body that is not UTF-8 is still refused by the kernel, as before: the
console takes text.
