// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! inputd (Phase 25): ring 3 virtio-input driver. One process per
//! device (keyboard, mouse). Receives the driver contract from init and,
//! as an extra, `SYSTEM` (`READ`) for timeouts. Queues the device events
//! and delivers them over IPC (`InputServiceProtocol v1`).
#![no_std]
#![no_main]

use aricode_input::Event;
use aricode_proto::input::{Event as WireEvent, Op, Reply, Request, Status, MAX_EVENTS, MAX_NAME};
use aricode_rt::sys::virtio_pci::{extra_handle, DmaRegion, Queue, Transport, IRQ, SERVICE};
use aricode_rt::sys::wait::{self, Deadline, Woken};
use aricode_rt::sys::{self, abi, Handle};

/// `virtio_input_config`: select, subsel, size and the payload at offset 8.
const CFG_SELECT: u64 = 0;
const CFG_SUBSEL: u64 = 1;
const CFG_SIZE: u64 = 2;
const CFG_PAYLOAD: u64 = 8;
const CFG_ID_NAME: u8 = 0x01;

const SLOTS: u16 = 64;
const RING: usize = 128;
/// Exit code when the end-of-round wait fails for a reason other than the
/// service endpoint being gone: looping on it would spin (ADR 0044).
const EXIT_WAIT_FAILED: u64 = 10;

fn say(msg: &[u8]) {
    sys::print(b"virtio-input: ");
    sys::print(msg);
    sys::print(b"\n");
}

struct Device {
    transport: Transport,
    eventq: Queue,
    buffers: DmaRegion,
    name: [u8; MAX_NAME],
    name_len: usize,
}

impl Device {
    fn setup(mask: u64) -> Result<Self, u64> {
        let transport = Transport::open(mask, 0).map_err(|e| e.code())?;
        // Device name.
        transport.device_cfg.w8(CFG_SELECT, CFG_ID_NAME);
        transport.device_cfg.w8(CFG_SUBSEL, 0);
        let size = transport.device_cfg.r8(CFG_SIZE) as usize;
        let mut name = [0u8; MAX_NAME];
        let mut name_len = size.min(MAX_NAME);
        for (i, b) in name[..name_len].iter_mut().enumerate() {
            *b = transport.device_cfg.r8(CFG_PAYLOAD + i as u64);
        }
        // `size` may include the terminator or padding.
        while name_len > 0 && matches!(name[name_len - 1], 0 | b' ') {
            name_len -= 1;
        }
        transport.device_cfg.w8(CFG_SELECT, 0);
        let mut eventq = transport.setup_queue(0, SLOTS).map_err(|e| e.code())?;
        let buffers = DmaRegion::alloc(4096).map_err(|_| 30u64)?;
        for i in 0..eventq.vq.size().min(SLOTS) {
            eventq
                .vq
                .add(&[(buffers.phys + 8 * i as u64, Event::LEN as u32, true)])
                .ok_or(31u64)?;
        }
        transport.driver_ok();
        eventq.notify(&transport);
        Ok(Self {
            transport,
            eventq,
            buffers,
            name,
            name_len,
        })
    }

    fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("?")
    }

    /// Next used slot, refilled: `Some(None)` for one that held no valid
    /// event, so a caller draining the queue does not stop there and leave
    /// the events behind it for an interrupt that already came; `None`
    /// once the queue is empty.
    fn poll(&mut self) -> Option<Option<Event>> {
        let (head, len) = self.eventq.vq.pop_used()?;
        let off = 8 * head as u64;
        let ev = if len as usize >= Event::LEN {
            Event::decode(self.buffers.slice_mut(off, Event::LEN as u64))
        } else {
            None
        };
        let _ = self
            .eventq
            .vq
            .add(&[(self.buffers.phys + off, Event::LEN as u32, true)]);
        self.eventq.notify(&self.transport);
        Some(ev)
    }
}

struct Clock(Handle);

impl Clock {
    fn now(&self) -> u64 {
        let mut b = [0u8; 8];
        match sys::system_info(self.0, abi::sysinfo::UPTIME, &mut b) {
            Ok(8) => u64::from_le_bytes(b),
            _ => 0,
        }
    }
}

/// Queue of events pending delivery (the oldest are dropped).
struct EventRing {
    buf: [(Event, u64); RING],
    head: usize,
    len: usize,
    dropped: u64,
}

impl EventRing {
    fn push(&mut self, ev: Event, at: u64) {
        if self.len == RING {
            self.head = (self.head + 1) % RING;
            self.len -= 1;
            self.dropped += 1;
        }
        self.buf[(self.head + self.len) % RING] = (ev, at);
        self.len += 1;
    }

    fn take(&mut self, out: &mut [WireEvent; MAX_EVENTS], times: &mut [u64; MAX_EVENTS]) -> usize {
        let n = self.len.min(MAX_EVENTS);
        for (slot, t) in out.iter_mut().zip(times.iter_mut()).take(n) {
            let (e, at) = self.buf[self.head];
            *slot = WireEvent {
                kind: e.kind,
                code: e.code,
                value: e.value,
            };
            *t = at;
            self.head = (self.head + 1) % RING;
            self.len -= 1;
        }
        n
    }
}

fn answer(reply_to: Handle, reply: Reply) {
    let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
    let _ = sys::handle_close(reply_to);
}

/// Requests served per round between two looks at the device, so a client
/// in a loop cannot keep events from being collected; a budget used up
/// makes the next round follow at once (ADR 0044).
const REQUESTS_PER_ROUND: usize = 8;

/// The driver's state between rounds: the device, its queued events and
/// the one read waiting for them.
struct Driver {
    dev: Device,
    ring: EventRing,
    pending: Option<(Handle, u64)>,
    events: [WireEvent; MAX_EVENTS],
    times: [u64; MAX_EVENTS],
}

impl Driver {
    /// Serves one request: the reply endpoint comes first; any other
    /// handle is closed.
    fn serve(&mut self, msg: &[u8], handles: &[Handle], now: u64) {
        let Some((&reply_to, extra)) = handles.split_first() else {
            return;
        };
        for h in extra {
            let _ = sys::handle_close(*h);
        }
        match Request::decode(msg) {
            Some(Request { op: Op::Info, .. }) => {
                let reply = Reply::status(Status::Ok)
                    .with_name(self.dev.name_str())
                    .unwrap_or(Reply::status(Status::Ok));
                answer(reply_to, reply);
            }
            Some(Request {
                op: Op::Read,
                timeout_ms,
            }) => {
                if self.ring.len > 0 {
                    let n = self.ring.take(&mut self.events, &mut self.times);
                    answer(reply_to, Reply::events(&self.events[..n], &self.times[..n]));
                } else if self.pending.is_some() {
                    answer(reply_to, Reply::status(Status::Busy));
                } else {
                    self.pending = Some((reply_to, now + timeout_ms.max(1) as u64));
                }
            }
            None => answer(reply_to, Reply::status(Status::Invalid)),
        }
    }

    /// Answers the waiting read once events arrived or its deadline passed.
    fn settle_pending(&mut self, now: u64) {
        let Some((reply_to, deadline)) = self.pending else {
            return;
        };
        if self.ring.len > 0 {
            let n = self.ring.take(&mut self.events, &mut self.times);
            answer(reply_to, Reply::events(&self.events[..n], &self.times[..n]));
            self.pending = None;
        } else if now >= deadline {
            answer(reply_to, Reply::status(Status::Timeout));
            self.pending = None;
        }
    }
}

fn main(mask: u64) -> u64 {
    let dev = match Device::setup(mask) {
        Ok(d) => d,
        Err(step) => {
            say(b"initialization failed");
            return step;
        }
    };
    let clock = Clock(extra_handle(mask, 0));
    sys::print(b"virtio-input: device ready: ");
    sys::print(dev.name_str().as_bytes());
    sys::print(b"\n");
    let mut drv = Driver {
        dev,
        ring: EventRing {
            buf: [(Event::default(), 0); RING],
            head: 0,
            len: 0,
            dropped: 0,
        },
        pending: None,
        events: [WireEvent::default(); MAX_EVENTS],
        times: [0u64; MAX_EVENTS],
    };
    let mut msg = [0u8; Request::LEN];
    let mut handles = [Handle(0); abi::IPC_MAX_HANDLES];
    // One wait for a request, an event (the device's interrupt) or the
    // waiting read's deadline (ADR 0044).
    let mut members = wait::Members::new();
    members.push(SERVICE);
    members.push(IRQ);
    say(b"serving requests");
    loop {
        let now = clock.now();
        while let Some(ev) = drv.dev.poll() {
            if let Some(ev) = ev {
                drv.ring.push(ev, now);
            }
        }
        let mut requests_left = true;
        for _ in 0..REQUESTS_PER_ROUND {
            let (n, nh) = match sys::ipc_receive(SERVICE, &mut msg, &mut handles, 0) {
                Ok(r) => r,
                // The service endpoint was closed or revoked (ADR 0030): nobody
                // can send here again, so polling it would spin.
                Err(e) if e.endpoint_gone() => {
                    say(b"service endpoint closed; exiting");
                    sys::process_exit(9);
                }
                Err(_) => {
                    requests_left = false;
                    break;
                }
            };
            drv.serve(&msg[..n], &handles[..nh], now);
        }
        drv.settle_pending(now);
        let next = match drv.pending {
            Some((_, deadline)) => Deadline::at(deadline),
            None => Deadline::NONE,
        };
        match wait::end_of_round(
            members.as_slice(),
            clock.now(),
            requests_left,
            next,
            &mut msg,
            &mut handles,
        ) {
            Ok(Woken::Message {
                len, handles: nh, ..
            }) => drv.serve(&msg[..len], &handles[..nh], clock.now()),
            Ok(Woken::Irq { .. } | Woken::Round) => {}
            Err(e) if e.endpoint_gone() => {
                say(b"service endpoint closed; exiting");
                sys::process_exit(9);
            }
            Err(e) => {
                aricode_rt::print_error(b"virtio-input: end-of-round wait", e);
                sys::process_exit(EXIT_WAIT_FAILED);
            }
        }
    }
}

aricode_rt::entry!(main);
