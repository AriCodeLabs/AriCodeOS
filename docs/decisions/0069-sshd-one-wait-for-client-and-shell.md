# ADR 0069: sshd waits once for the client and the shell

- **Date:** 2026-10-31
- **Status:** Accepted. Follow-up of ADR 0068's limits; applies ADR 0044's
  rule (one wait that ends on whichever comes first) to sshd.

## Context

sshd's connection loop had two things to wait for:
- bytes from the client, through the network service's `tcp_recv`, a call
  that blocks in netd until data or its timeout;
- the session shell's output, on an IPC endpoint.

It could wait on only one. It asked netd for data with a 20 ms timeout
(1 ms while output was left), then polled the shell's output. Output the
shell wrote during the receive waited for it to end, and so did the echo
of a typed key, which comes back from the shell.

Measured inside AriCodeOS under QEMU TCG, with OpenSSH on the host
(`ssh -p 17022`):

| | Before |
|---|---|
| Connection plus `ls` | 181 ms |
| Output, `repeat 1000 help` (1.6 MB) | 0.41 MB/s |
| Echo of a typed key on a pty, median (max) | 30.6 ms (41 ms) |

A throwaway build with the receive timeout at 1 ms brought the echo to
10.5 ms: the wait was the cause. Polling every millisecond is ADR 0044's
rejected alternative (CPU for latency, and the wakeup still missed), so
it was not kept.

## Decision

1. **The receive is split in two** in the SDK (`NetClient::tcp_recv_start`
   and `tcp_recv_finish`).
   - `tcp_recv_start` sends the request with its own reply endpoint
     (`rpc::send`) and returns that endpoint without waiting.
   - `tcp_recv_finish` decodes the reply and copies the data.
   - `tcp_recv` is now the two halves with a bounded receive in between,
     so there is one implementation.

   netd's protocol does not change: it already parked the receive and
   answered on the request's reply endpoint.
2. **sshd keeps one receive waiting in netd**, on a second `NetClient`
   with a DMA buffer of its own. The pending receive owns that buffer
   until its reply, and sends keep using the first client.
3. **One wait per pass** (`ipc_receive_any`) on the receive's reply
   endpoint and, while the client's window has room, the shell's output
   endpoint. It ends on whichever comes first. The message it took is
   handled in the next pass:
   - shell output first, before the buffer is reused;
   - the client's bytes before more output is taken.

   The pass skips the wait while it left work: bytes just received, or a
   full burst of output with room left.
4. **The wait's bounds.**
   - A closed member does not end a wait on two members (ADR 0044,
     point 3). So while a session runs, the wait is bounded by
     `SESSION_CHECK_MS` (1 s), and a shell that died is noticed within
     it, as `ssh.shell_fault` checks.
   - While typed input waits for room in the shell's queue, the bound is
     `INPUT_RETRY_MS` (10 ms).
   - Before authentication, the bound is the authentication deadline.
   - Otherwise there is none.
   - netd answers a receive after `RECV_WAIT_MS` (60 s) at most, and sshd
     asks again.

## Consequences

Measured the same way:

| | Before | After |
|---|---|---|
| Connection plus `ls` | 181 ms | 120 ms |
| Output, `repeat 1000 help` | 0.41 MB/s | 3.76 MB/s |
| Echo of a typed key, median (max) | 30.6 ms (41 ms) | 9.7 ms (12 ms) |

Output improved too: the shell no longer waits for sshd to come back from
a receive before its next message is taken. With an idle interactive
session open, the CPU stayed 97% idle (`cpus --detail`).

## Limits

- A shell that dies is noticed within 1 s, against 20 ms before. A wait
  that reports one closed member among open ones would need an ABI
  change.
- Each line the shell prints is still one IPC message. Coalescing them in
  the session shell would need a flush before every point where it
  blocks.

## Tests

- QEMU: the `ssh` and `network` areas, 45 scenarios: `ssh.exec`,
  `ssh.shell_fault` (a dying shell ends the session with status 139),
  the file service and key scenarios, and every user of `tcp_recv`
  through the SDK.
- The measurements above: `guest-bench.py` in the scratch harness, before
  and after, same build mode and QEMU.
