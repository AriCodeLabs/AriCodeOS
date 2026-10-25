// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Input service (`aricode-proto::input`): the client, and the queue of
//! events a driver keeps for it.

use crate::{rpc, Error, Handle};
pub use aricode_proto::input::Event;
use aricode_proto::input::{Op, Reply, Request, Status, MAX_EVENTS, MAX_READ_MS};

pub struct InputClient {
    service: Handle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    Ipc(Error),
    Status(Status),
}

impl InputError {
    /// The input service endpoint has nobody to receive any more
    /// (`Error::ServiceGone`, ADR 0030): the device is gone for good.
    pub fn service_gone(&self) -> bool {
        *self == InputError::Ipc(Error::ServiceGone)
    }

    /// The driver dropped this request (it died, `Error::ReplyLost`): its
    /// supervisor relaunches it on the same endpoint, so read again after a
    /// short pause.
    pub fn reply_lost(&self) -> bool {
        *self == InputError::Ipc(Error::ReplyLost)
    }
}

impl InputClient {
    pub fn new(service: Handle) -> Result<Self, Error> {
        Ok(Self { service })
    }

    /// Slack over the requested deadline so a dead service does not leave the
    /// client waiting forever (audit 4836023, finding 1).
    const REPLY_GRACE_MS: u64 = 5000;

    fn call(&self, req: Request) -> Result<Reply, InputError> {
        let mut msg = [0u8; Reply::LEN];
        let wait = req.timeout_ms as u64 + Self::REPLY_GRACE_MS;
        let len = rpc::call(
            self.service,
            &req.encode(),
            &[],
            Self::REPLY_GRACE_MS,
            wait,
            &mut msg,
        )
        .map_err(InputError::Ipc)?;
        let reply = Reply::decode(&msg[..len]).ok_or(InputError::Ipc(Error::Invalid))?;
        match reply.status {
            Status::Ok => Ok(reply),
            s => Err(InputError::Status(s)),
        }
    }

    /// Device name, copied into `out`; returns its length.
    pub fn name(&self, out: &mut [u8; 32]) -> Result<usize, InputError> {
        let r = self.call(Request {
            op: Op::Info,
            timeout_ms: 0,
        })?;
        let n = r.name_len as usize;
        out[..n].copy_from_slice(&r.name[..n]);
        Ok(n)
    }

    /// Waits for events up to `timeout_ms`; returns how many are in `out`,
    /// with the timestamp (ms since boot) of each one in `times`.
    pub fn read(
        &self,
        timeout_ms: u32,
        out: &mut [Event; MAX_EVENTS],
        times: &mut [u64; MAX_EVENTS],
    ) -> Result<usize, InputError> {
        let r = self.call(Request {
            op: Op::Read,
            timeout_ms,
        })?;
        Ok(Self::copy_events(&r, out, times))
    }

    /// Starts a read of up to `timeout_ms` without waiting for it: the
    /// reply comes on the returned endpoint (receive side), which the
    /// caller waits on, several devices at once with `ipc_receive_any`
    /// (ADR 0044), hands to `read_reply` and closes. A client that keeps
    /// one such read outstanding per device sleeps until a device has
    /// events instead of polling each in turn.
    pub fn start_read(&self, timeout_ms: u32) -> Result<Handle, InputError> {
        let req = Request {
            op: Op::Read,
            timeout_ms,
        };
        rpc::send(self.service, &req.encode(), &[], Self::REPLY_GRACE_MS).map_err(InputError::Ipc)
    }

    /// The events of a started read's reply `msg`, as `read` gives them.
    pub fn read_reply(
        msg: &[u8],
        out: &mut [Event; MAX_EVENTS],
        times: &mut [u64; MAX_EVENTS],
    ) -> Result<usize, InputError> {
        let reply = Reply::decode(msg).ok_or(InputError::Ipc(Error::Invalid))?;
        match reply.status {
            Status::Ok => Ok(Self::copy_events(&reply, out, times)),
            s => Err(InputError::Status(s)),
        }
    }

    fn copy_events(
        r: &Reply,
        out: &mut [Event; MAX_EVENTS],
        times: &mut [u64; MAX_EVENTS],
    ) -> usize {
        let n = (r.count as usize).min(MAX_EVENTS);
        out[..n].copy_from_slice(&r.events[..n]);
        times[..n].copy_from_slice(&r.times[..n]);
        n
    }
}

/// Events an input driver keeps for its client.
pub const QUEUE_LEN: usize = 128;

/// An input driver's side of the service (ADR 0013): the events received
/// and not yet read, and the one read waiting for them.
pub struct EventQueue {
    buf: [(Event, u64); QUEUE_LEN],
    head: usize,
    len: usize,
    dropped: u64,
    waiting: Option<(Handle, u64)>,
    /// Replies not delivered because their reader was gone, and whether
    /// the first was told (`first_gone_reader`).
    gone: u64,
    gone_told: bool,
}

impl Default for EventQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl EventQueue {
    pub const fn new() -> Self {
        Self {
            buf: [(
                Event {
                    kind: 0,
                    code: 0,
                    value: 0,
                },
                0,
            ); QUEUE_LEN],
            head: 0,
            len: 0,
            dropped: 0,
            waiting: None,
            gone: 0,
            gone_told: false,
        }
    }

    /// Queues `ev`, received at `at` (ms since boot). When full the oldest
    /// event is dropped and `true` returned.
    pub fn push(&mut self, ev: Event, at: u64) -> bool {
        let full = self.len == QUEUE_LEN;
        if full {
            self.head = (self.head + 1) % QUEUE_LEN;
            self.len -= 1;
            self.dropped += 1;
        }
        self.buf[(self.head + self.len) % QUEUE_LEN] = (ev, at);
        self.len += 1;
        full
    }

    /// Events dropped so far because the queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// `true` once: after the first time events could not be delivered
    /// because their reader was gone (they stayed queued), for the driver
    /// to say so.
    pub fn first_gone_reader(&mut self) -> bool {
        let first = self.gone > 0 && !self.gone_told;
        self.gone_told |= first;
        first
    }

    /// When the waiting read ends without events, if one waits.
    pub fn deadline(&self) -> Option<u64> {
        self.waiting.map(|(_, deadline)| deadline)
    }

    /// Serves a read whose reply goes to `reply_to`: the queued events at
    /// once, or it waits up to `timeout_ms` for some. The service has one
    /// place for a waiting read; another is answered `Busy`.
    pub fn read(&mut self, reply_to: Handle, timeout_ms: u32, now: u64) {
        self.read_with(reply_to, timeout_ms, now, &mut answer);
    }

    /// Answers the waiting read once events arrived or its deadline passed.
    pub fn settle(&mut self, now: u64) {
        self.settle_with(now, &mut answer);
    }

    fn read_with(
        &mut self,
        reply_to: Handle,
        timeout_ms: u32,
        now: u64,
        send: &mut impl FnMut(Handle, &Reply) -> bool,
    ) {
        if self.len > 0 {
            self.deliver(reply_to, send);
        } else if self.waiting.is_some() {
            send(reply_to, &Reply::status(Status::Busy));
        } else {
            let wait = timeout_ms.clamp(1, MAX_READ_MS);
            self.waiting = Some((reply_to, now + wait as u64));
        }
    }

    fn settle_with(&mut self, now: u64, send: &mut impl FnMut(Handle, &Reply) -> bool) {
        let Some((reply_to, deadline)) = self.waiting else {
            return;
        };
        if self.len > 0 {
            self.waiting = None;
            self.deliver(reply_to, send);
        } else if now >= deadline {
            self.waiting = None;
            send(reply_to, &Reply::status(Status::Timeout));
        }
    }

    /// Sends the oldest events to `reply_to`. They leave the queue only
    /// once delivered: a reader that died with its read waiting takes
    /// nothing, the next read gets them (audit 2026-10-25 C, M4).
    fn deliver(&mut self, reply_to: Handle, send: &mut impl FnMut(Handle, &Reply) -> bool) {
        let n = self.len.min(MAX_EVENTS);
        let mut events = [Event::default(); MAX_EVENTS];
        let mut times = [0u64; MAX_EVENTS];
        for i in 0..n {
            (events[i], times[i]) = self.buf[(self.head + i) % QUEUE_LEN];
        }
        if send(reply_to, &Reply::events(&events[..n], &times[..n])) {
            self.head = (self.head + n) % QUEUE_LEN;
            self.len -= n;
        } else {
            self.gone += 1;
        }
    }
}

/// Sends `reply` on the one-shot reply endpoint `reply_to` and closes it;
/// `false` if it was not delivered (its reader is gone).
fn answer(reply_to: Handle, reply: &Reply) -> bool {
    let sent = crate::ipc_send(reply_to, &reply.encode(), &[], 0).is_ok();
    let _ = crate::handle_close(reply_to);
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: u16) -> Event {
        Event {
            kind: 1,
            code,
            value: 1,
        }
    }

    /// Records every answer; delivers while `alive`.
    struct Reader {
        alive: bool,
        got: [(u32, Status, u8); 8],
        n: usize,
    }

    impl Reader {
        fn new(alive: bool) -> Self {
            Self {
                alive,
                got: [(0, Status::Ok, 0); 8],
                n: 0,
            }
        }

        fn send(&mut self) -> impl FnMut(Handle, &Reply) -> bool + '_ {
            |to, r| {
                self.got[self.n] = (to.0, r.status, r.count);
                self.n += 1;
                self.alive
            }
        }
    }

    #[test]
    fn queued_events_are_answered_at_once() {
        let mut q = EventQueue::new();
        for c in 0..6 {
            q.push(key(c), 10);
        }
        let mut r = Reader::new(true);
        q.read_with(Handle(7), 1_000, 0, &mut r.send());
        assert_eq!(r.got[0], (7, Status::Ok, MAX_EVENTS as u8));
        assert_eq!(q.len, 2);
        assert_eq!(q.deadline(), None);
    }

    #[test]
    fn a_waiting_read_gets_the_next_events() {
        let mut q = EventQueue::new();
        let mut r = Reader::new(true);
        q.read_with(Handle(7), 1_000, 100, &mut r.send());
        assert_eq!(q.deadline(), Some(1_100));
        q.read_with(Handle(8), 1_000, 100, &mut r.send());
        assert_eq!(r.got[0], (8, Status::Busy, 0));
        q.push(key(30), 150);
        q.settle_with(150, &mut r.send());
        assert_eq!(r.got[1], (7, Status::Ok, 1));
        assert_eq!((q.len, q.deadline()), (0, None));
    }

    #[test]
    fn a_waiting_read_ends_at_its_deadline() {
        let mut q = EventQueue::new();
        let mut r = Reader::new(true);
        q.read_with(Handle(7), 50, 100, &mut r.send());
        q.settle_with(149, &mut r.send());
        assert_eq!(r.n, 0);
        q.settle_with(150, &mut r.send());
        assert_eq!(r.got[0], (7, Status::Timeout, 0));
        assert_eq!(q.deadline(), None);
    }

    #[test]
    fn a_long_read_is_cut_to_the_maximum() {
        let mut q = EventQueue::new();
        let mut r = Reader::new(true);
        q.read_with(Handle(7), u32::MAX, 100, &mut r.send());
        assert_eq!(q.deadline(), Some(100 + MAX_READ_MS as u64));
        let mut q = EventQueue::new();
        q.read_with(Handle(7), 0, 100, &mut r.send());
        assert_eq!(q.deadline(), Some(101));
    }

    #[test]
    fn a_dead_reader_takes_no_events() {
        // The reader died with its read waiting: the events it was sent
        // stay for the next read, and the place is free again.
        let mut q = EventQueue::new();
        let mut dead = Reader::new(false);
        q.read_with(Handle(7), 10_000, 0, &mut dead.send());
        q.push(key(30), 5);
        q.push(key(31), 6);
        q.settle_with(5, &mut dead.send());
        assert_eq!(dead.got[0], (7, Status::Ok, 2));
        assert_eq!((q.len, q.deadline()), (2, None));
        assert!(q.first_gone_reader());
        assert!(!q.first_gone_reader());
        let mut next = Reader::new(true);
        q.read_with(Handle(9), 10_000, 7, &mut next.send());
        assert_eq!(next.got[0], (9, Status::Ok, 2));
        assert_eq!(q.len, 0);
    }

    #[test]
    fn a_full_queue_drops_the_oldest() {
        let mut q = EventQueue::new();
        for c in 0..QUEUE_LEN as u16 {
            assert!(!q.push(key(c), 0));
        }
        assert!(q.push(key(999), 1));
        assert_eq!(q.dropped(), 1);
        assert_eq!(q.buf[q.head].0.code, 1);
        assert_eq!(q.buf[(q.head + q.len - 1) % QUEUE_LEN].0.code, 999);
    }
}
