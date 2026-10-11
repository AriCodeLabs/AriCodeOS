//! inputd (Phase 25): ring 3 virtio-input driver. One process per
//! device (keyboard, mouse). Receives the driver contract from init and,
//! as an extra, `SYSTEM` (`READ`) for timeouts. Queues the device events
//! and delivers them over IPC (`InputServiceProtocol v1`).
#![no_std]
#![no_main]

use aricode_input::Event;
use aricode_proto::input::{Event as WireEvent, Op, Reply, Request, Status, MAX_EVENTS, MAX_NAME};
use aricode_rt::sys::virtio_pci::{extra_handle, DmaRegion, Queue, Transport, IRQ, SERVICE};
use aricode_rt::sys::{self, abi, Handle};

/// `virtio_input_config`: select, subsel, size and the payload at offset 8.
const CFG_SELECT: u64 = 0;
const CFG_SUBSEL: u64 = 1;
const CFG_SIZE: u64 = 2;
const CFG_PAYLOAD: u64 = 8;
const CFG_ID_NAME: u8 = 0x01;

const SLOTS: u16 = 64;
const RING: usize = 128;

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

    /// Next device event; refills the slot.
    fn poll(&mut self) -> Option<Event> {
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
        ev
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

fn main(mask: u64) -> u64 {
    let mut dev = match Device::setup(mask) {
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
    let mut ring = EventRing {
        buf: [(Event::default(), 0); RING],
        head: 0,
        len: 0,
        dropped: 0,
    };
    let mut pending: Option<(Handle, u64)> = None;
    let mut msg = [0u8; Request::LEN];
    let mut handles = [Handle(0); 1];
    let mut events = [WireEvent::default(); MAX_EVENTS];
    let mut times = [0u64; MAX_EVENTS];
    say(b"serving requests");
    loop {
        let now = clock.now();
        while let Some(ev) = dev.poll() {
            ring.push(ev, now);
        }
        for _ in 0..8 {
            let Ok((n, nh)) = sys::ipc_receive(SERVICE, &mut msg, &mut handles, 0) else {
                break;
            };
            if nh == 0 {
                continue;
            }
            let reply_to = handles[0];
            match Request::decode(&msg[..n]) {
                Some(Request { op: Op::Info, .. }) => {
                    let reply = Reply::status(Status::Ok)
                        .with_name(dev.name_str())
                        .unwrap_or(Reply::status(Status::Ok));
                    answer(reply_to, reply);
                }
                Some(Request {
                    op: Op::Read,
                    timeout_ms,
                }) => {
                    if ring.len > 0 {
                        let n = ring.take(&mut events, &mut times);
                        answer(reply_to, Reply::events(&events[..n], &times[..n]));
                    } else if pending.is_some() {
                        answer(reply_to, Reply::status(Status::Busy));
                    } else {
                        pending = Some((reply_to, now + timeout_ms.max(1) as u64));
                    }
                }
                None => answer(reply_to, Reply::status(Status::Invalid)),
            }
        }
        if let Some((reply_to, deadline)) = pending {
            if ring.len > 0 {
                let n = ring.take(&mut events, &mut times);
                answer(reply_to, Reply::events(&events[..n], &times[..n]));
                pending = None;
            } else if now >= deadline {
                answer(reply_to, Reply::status(Status::Timeout));
                pending = None;
            }
        }
        let _ = sys::irq_wait(IRQ, 10);
    }
}

aricode_rt::entry!(main);
