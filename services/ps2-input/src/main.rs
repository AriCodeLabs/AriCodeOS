// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! ps2_input (ADR 0075): the ring-3 decoder of the PS/2 keyboard and
//! pointer. The kernel owns the i8042 and, while a process holds the
//! screen, hands its bytes over untouched through the PS/2 stream; this
//! service turns them into evdev events (keyboard: scancode set 1; pointer:
//! 3- or 4-byte packets) and delivers them over IPC
//! (`InputServiceProtocol v1`), like the other input drivers.
//!
//! Handles, from init: its service endpoint, the PS/2 stream (`READ |
//! WAIT`) and `SYSTEM` (`READ`) for timestamps.
#![no_std]
#![no_main]

use abi::ps2;
use aricode_input::{Event, KeysDown, Ps2Mouse, ScancodeSet1, EV_SYN, MAX_REPORT_EVENTS};
use aricode_proto::input::{
    Event as WireEvent, Op, Reply, Request, Status, KIND_KEYBOARD, KIND_POINTER,
};
use aricode_rt::sys::input::EventQueue;
use aricode_rt::sys::wait::{self, Deadline, Woken};
use aricode_rt::sys::{self, abi, Handle};

const SERVICE: Handle = Handle(0x0001_0004);
const STREAM: Handle = Handle(0x0001_0005);
const SYSTEM: Handle = Handle(0x0001_0006);

const NAME: &str = "PS/2 keyboard and pointer";
/// Stream reads per round: a stream that keeps filling cannot keep the
/// requests from being served; what is left makes the next round follow at
/// once (ADR 0044).
const READS_PER_ROUND: usize = 4;
/// Requests served per round between two looks at the stream.
const REQUESTS_PER_ROUND: usize = 8;
/// Exit code when the stream cannot be read (its handle revoked).
const EXIT_STREAM_GONE: u64 = 11;
/// Exit code when the end-of-round wait fails for a reason other than the
/// service endpoint being gone: looping on it would spin (ADR 0044).
const EXIT_WAIT_FAILED: u64 = 10;

fn say(msg: &[u8]) {
    sys::print(b"ps2-input: ");
    sys::print(msg);
    sys::print(b"\n");
}

fn now() -> u64 {
    let mut b = [0u8; 8];
    match sys::system_info(SYSTEM, abi::sysinfo::UPTIME, &mut b) {
        Ok(8) => u64::from_le_bytes(b),
        _ => 0,
    }
}

fn answer(reply_to: Handle, reply: Reply) {
    let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
    let _ = sys::handle_close(reply_to);
}

/// The decoders and the events waiting for the reader.
struct Driver {
    keyboard: ScancodeSet1,
    keys: KeysDown,
    mouse: Ps2Mouse,
    queue: EventQueue,
    /// `input::KIND_*` bits, from what the kernel found on the controller.
    kinds: u8,
    /// When the last aux byte was taken (ms since boot).
    last_aux_ms: u64,
}

/// A pause between two aux bytes longer than this ends any packet: a
/// pointer sends a packet's bytes back to back, so a decoder still waiting
/// for the rest of one lost a byte (Linux's `psmouse` resynchronizes after
/// half a second too). Bit 3 alone can stay out of step while motion bytes
/// happen to have it set.
const AUX_GAP_MS: u64 = 500;

impl Driver {
    fn push(&mut self, ev: Event, at: u64) {
        let wire = WireEvent {
            kind: ev.kind,
            code: ev.code,
            value: ev.value,
        };
        if self.queue.push(wire, at) && self.queue.dropped() == 1 {
            say(b"input event queue full: the oldest events are dropped");
        }
    }

    fn push_all(&mut self, events: &[Event], at: u64) {
        for ev in events {
            self.push(*ev, at);
        }
    }

    /// The stream was cut: the decoders start over and every key and
    /// button held is released, so the reader keeps nothing pressed.
    fn reset(&mut self, at: u64) {
        self.keyboard = ScancodeSet1::new();
        let mut released = false;
        let mut keys = self.keys;
        keys.release_all(|ev| {
            self.push(ev, at);
            released = true;
        });
        self.keys = keys;
        if released {
            self.push(SYN, at);
        }
        let mut out = [Event::default(); MAX_REPORT_EVENTS];
        let n = self.mouse.release_all(&mut out);
        self.push_all(&out[..n], at);
    }

    fn entry(&mut self, tag: u8, byte: u8, at: u64) {
        match tag {
            ps2::TAG_RESET => self.reset(at),
            ps2::TAG_KEYBOARD => {
                if let Some(ev) = self.keyboard.feed(byte) {
                    self.keys.follow(&ev);
                    self.push(ev, at);
                    self.push(SYN, at);
                }
            }
            ps2::TAG_AUX => {
                if at.saturating_sub(self.last_aux_ms) > AUX_GAP_MS {
                    self.mouse.resync();
                }
                self.last_aux_ms = at;
                let mut out = [Event::default(); MAX_REPORT_EVENTS];
                let n = self.mouse.feed(byte, &mut out);
                self.push_all(&out[..n], at);
            }
            _ => {}
        }
    }

    /// Takes what the stream holds, at most `READS_PER_ROUND` reads;
    /// whether entries may be left.
    fn drain(&mut self, at: u64) -> bool {
        let mut buf = [0u8; 128 * ps2::ENTRY];
        for _ in 0..READS_PER_ROUND {
            let n = match sys::ps2_read(STREAM, &mut buf) {
                Ok(n) => n,
                Err(e) => {
                    aricode_rt::print_error(b"ps2-input: the PS/2 stream cannot be read", e);
                    sys::process_exit(EXIT_STREAM_GONE);
                }
            };
            for &[tag, byte] in buf[..n].as_chunks::<{ ps2::ENTRY }>().0 {
                self.entry(tag, byte, at);
            }
            if n < buf.len() {
                return false;
            }
        }
        true
    }

    fn tell_gone_reader(&mut self) {
        if self.queue.first_gone_reader() {
            say(b"a reader died with its read waiting; its events wait for the next read");
        }
    }

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
                    .with_name(NAME)
                    .unwrap_or(Reply::status(Status::Ok))
                    .with_kinds(self.kinds);
                answer(reply_to, reply);
            }
            Some(Request {
                op: Op::Read,
                timeout_ms,
            }) => {
                self.queue.read(reply_to, timeout_ms, now);
                self.tell_gone_reader();
            }
            None => answer(reply_to, Reply::status(Status::Invalid)),
        }
    }
}

const SYN: Event = Event {
    kind: EV_SYN,
    code: 0,
    value: 0,
};

fn main(_arg: u64) -> u64 {
    let found = match sys::object_id(STREAM) {
        Ok(found) => found,
        Err(e) => {
            aricode_rt::print_error(b"ps2-input: started without the PS/2 stream", e);
            return 1;
        }
    };
    let mut kinds = 0;
    if found & ps2::KIND_KEYBOARD != 0 {
        kinds |= KIND_KEYBOARD;
    }
    if found & ps2::KIND_POINTER != 0 {
        kinds |= KIND_POINTER;
    }
    let mut drv = Driver {
        keyboard: ScancodeSet1::new(),
        keys: KeysDown::new(),
        mouse: Ps2Mouse::new(found & ps2::KIND_WHEEL != 0),
        queue: EventQueue::new(),
        kinds,
        last_aux_ms: 0,
    };
    sys::print(b"ps2-input: ");
    sys::print(
        match (kinds & KIND_KEYBOARD != 0, kinds & KIND_POINTER != 0) {
            (true, true) => b"keyboard and pointer",
            (true, false) => b"keyboard, no pointer",
            (false, true) => b"pointer, no keyboard",
            (false, false) => b"neither a keyboard nor a pointer answered",
        },
    );
    sys::print(if found & ps2::KIND_WHEEL != 0 {
        b" (wheel); serving requests\n"
    } else {
        b"; serving requests\n"
    });
    let mut msg = [0u8; Request::LEN];
    let mut handles = [Handle(0); abi::IPC_MAX_HANDLES];
    // One wait for a request, bytes on the stream or the waiting read's
    // deadline (ADR 0044).
    let mut members = wait::Members::new();
    members.push(SERVICE);
    members.push(STREAM);
    loop {
        let t = now();
        let stream_left = drv.drain(t);
        let mut requests_left = true;
        for _ in 0..REQUESTS_PER_ROUND {
            let (n, nh) = match sys::ipc_receive(SERVICE, &mut msg, &mut handles, 0) {
                Ok(r) => r,
                // The service endpoint was closed or revoked (ADR 0030):
                // nobody can send here again, so polling it would spin.
                Err(e) if e.endpoint_gone() => {
                    say(b"service endpoint closed; exiting");
                    sys::process_exit(9);
                }
                Err(_) => {
                    requests_left = false;
                    break;
                }
            };
            drv.serve(&msg[..n], &handles[..nh], t);
        }
        drv.queue.settle(t);
        drv.tell_gone_reader();
        let next = drv.queue.deadline().map_or(Deadline::NONE, Deadline::at);
        match wait::end_of_round(
            members.as_slice(),
            now(),
            requests_left || stream_left,
            next,
            &mut msg,
            &mut handles,
        ) {
            Ok(Woken::Message {
                len, handles: nh, ..
            }) => drv.serve(&msg[..len], &handles[..nh], now()),
            Ok(Woken::Irq { .. } | Woken::Round) => {}
            Err(e) if e.endpoint_gone() => {
                say(b"service endpoint closed; exiting");
                sys::process_exit(9);
            }
            Err(e) => {
                aricode_rt::print_error(b"ps2-input: end-of-round wait", e);
                sys::process_exit(EXIT_WAIT_FAILED);
            }
        }
    }
}

aricode_rt::entry!(main);
