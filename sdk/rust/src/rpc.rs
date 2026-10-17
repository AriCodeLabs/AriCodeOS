// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Request/reply call over a service endpoint (ADR 0012): each request
//! carries its own reply endpoint, which is closed when done, and every wait
//! is bounded. A late reply reaches a closed endpoint and is lost: it is
//! never attributed to another request.

use crate::{self as sys, rights, Error, Handle};

/// Extra handles a request may carry besides the reply one.
pub const MAX_EXTRA: usize = 3;

/// Sends `request` with the `extra` handles (which are consumed: they go to
/// the server or are closed if the send fails) and waits for the reply in
/// `out`. `send_wait_ms` bounds the server queue; `reply_wait_ms` the reply.
pub fn call(
    service: Handle,
    request: &[u8],
    extra: &[Handle],
    send_wait_ms: u64,
    reply_wait_ms: u64,
    out: &mut [u8],
) -> Result<usize, Error> {
    let mut none: [Handle; 0] = [];
    call_handles(
        service,
        request,
        extra,
        send_wait_ms,
        reply_wait_ms,
        out,
        &mut none,
    )
    .map(|(len, _)| len)
}

/// Like `call`, also collecting the handles the reply carries into
/// `handles_out`; returns `(bytes, handles)`.
pub fn call_handles(
    service: Handle,
    request: &[u8],
    extra: &[Handle],
    send_wait_ms: u64,
    reply_wait_ms: u64,
    out: &mut [u8],
    handles_out: &mut [Handle],
) -> Result<(usize, usize), Error> {
    if extra.len() > MAX_EXTRA {
        return Err(Error::Invalid);
    }
    let reply = sys::endpoint_create()?;
    let result = (|| {
        let reply_w = sys::handle_duplicate(reply, rights::WRITE | rights::TRANSFER)?;
        let mut handles = [Handle(0); MAX_EXTRA + 1];
        handles[0] = reply_w;
        handles[1..1 + extra.len()].copy_from_slice(extra);
        let sent = sys::ipc_send(service, request, &handles[..1 + extra.len()], send_wait_ms);
        if sent.is_err() {
            for h in &handles[..1 + extra.len()] {
                let _ = sys::handle_close(*h);
            }
        }
        sent?;
        sys::ipc_receive(reply, out, handles_out, reply_wait_ms)
    })();
    let _ = sys::handle_close(reply);
    result
}

/// Slack on top of the deadline a request asks for: what the service is
/// granted to answer when it is alive. Clients of services whose work is
/// bounded by a device use their own, longer reply waits
/// (`block::REPLY_WAIT_MS`, `fs::REPLY_WAIT_MS`).
pub const GRACE_MS: u64 = 5000;
