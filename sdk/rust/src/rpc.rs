// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Request/reply call over a service endpoint (ADR 0012): each request
//! carries its own reply endpoint, which is closed when done, and every wait
//! is bounded. A late reply reaches a closed endpoint and is lost: it is
//! never attributed to another request.
//!
//! A server that is gone is reported at once (ADR 0030): the client keeps
//! only the receive side of its reply endpoint and the send side travels
//! with the request. The two ways it ends mean different things:
//!
//! - `Error::ServiceGone`: the send was refused because the service
//!   endpoint has no receiver left (or was revoked). Permanent: stop using
//!   the endpoint.
//! - `Error::ReplyLost`: the request was accepted and dropped unanswered
//!   (the server died holding it, or it was discarded with a queue). Not
//!   permanent: init keeps the receive side of a supervised endpoint and
//!   relaunches the server on it, so the next request queues for the next
//!   generation. Retry after a short pause; if the service is given up, the
//!   retry gets `ServiceGone`.
//!
//! The full reply wait is only spent on a server that is alive and slow.

use crate::{self as sys, Error, Handle};

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
    let reply = send(service, request, extra, send_wait_ms)?;
    let result = sys::ipc_receive(reply, out, handles_out, reply_wait_ms).map_err(reply_error);
    let _ = sys::handle_close(reply);
    result
}

/// The first half of a call: sends `request` with its own reply endpoint
/// and the `extra` handles (consumed as in `call`) and returns the reply
/// endpoint's receive side, without waiting. The caller receives the reply
/// on it (several at once with `ipc_receive_any`), maps a failed receive
/// with `reply_error`, and closes it.
pub fn send(
    service: Handle,
    request: &[u8],
    extra: &[Handle],
    send_wait_ms: u64,
) -> Result<Handle, Error> {
    let give_up = |e: Error| {
        for h in extra {
            let _ = sys::handle_close(*h);
        }
        Err(e)
    };
    if extra.len() > MAX_EXTRA {
        return give_up(Error::Invalid);
    }
    let (reply, reply_w) = match sys::endpoint_create_pair() {
        Ok(pair) => pair,
        Err(e) => return give_up(e),
    };
    let mut handles = [Handle(0); MAX_EXTRA + 1];
    handles[0] = reply_w;
    handles[1..1 + extra.len()].copy_from_slice(extra);
    let sent = sys::ipc_send(service, request, &handles[..1 + extra.len()], send_wait_ms);
    if let Err(e) = sent {
        for h in &handles[..1 + extra.len()] {
            let _ = sys::handle_close(*h);
        }
        let _ = sys::handle_close(reply);
        return Err(match e {
            Error::PeerClosed => Error::ServiceGone,
            // `Revoked` also comes back when one of the extra handles was
            // revoked: only the service endpoint's own revocation means the
            // service is gone (asked on this error path only).
            Error::Revoked if sys::object_kind(service) == Err(Error::Revoked) => {
                Error::ServiceGone
            }
            e => e,
        });
    }
    Ok(reply)
}

/// A failed receive of a reply, named: `PeerClosed` there means the
/// server dropped the request (`ReplyLost`).
pub fn reply_error(e: Error) -> Error {
    match e {
        Error::PeerClosed => Error::ReplyLost,
        e => e,
    }
}

/// Slack on top of the deadline a request asks for: what the service is
/// granted to answer when it is alive. Clients of services whose work is
/// bounded by a device use their own, longer reply waits
/// (`block::REPLY_WAIT_MS`, `fs::REPLY_WAIT_MS`).
pub const GRACE_MS: u64 = 5000;
