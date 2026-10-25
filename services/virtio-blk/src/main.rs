// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Ring 3 virtio-blk driver (Phase 18) and block service (Phase 19).
//!
//! Receives from `init`, in this order, the service endpoint, the PCI
//! configuration space, the hardware capability, the MSI-X IRQ, the BARs
//! present (`arg` is the BAR mask) and, only when the command line has
//! `strict` or the test-only `blk_fault`, `SYSTEM` (read: the command line). Initializes the device through the
//! modern interface, creates queue 0 in DMA memory, reads the GPT and serves
//! requests from other processes over IPC (`aricode-proto::block`); they supply
//! their own DMA buffer: the device reads and writes it directly.
//!
//! A request the device does not complete (a timeout with its descriptors
//! still in the ring, an unexpected used entry, a device that asks for a
//! reset) leaves the device owning the client's buffer: the driver keeps
//! the buffer's handle, resets the device and creates the queue again,
//! answers `Io` (only then, so no late write lands in a buffer the client
//! reuses, incident 0033), and releases the buffer (incident 0017). After
//! `MAX_RESETS` resets with no request completed in between, or a reset
//! that fails, the driver exits: the kernel clears bus mastering before it
//! frees what the driver held (ADR 0038) and the supervisor restarts it.
//! With `strict` (ADR 0028) the first failed request halts the driver
//! instead, with the device and the kept buffer as the failure left them.
#![no_std]
#![no_main]

extern crate alloc;

use aricode_proto::block::{Op, Reply, Request, Status, MAX_SECTORS};
use aricode_rt::sys::block::{map_client_buffer, ClientBuffers};
use aricode_rt::sys::client_buffer::{DeviceOwned, Kept};
use aricode_rt::sys::virtio_pci::{extra_handle, DmaRegion, Queue, Transport, IRQ, SERVICE};
use aricode_rt::sys::{self, Error, Handle, TIMEOUT_INFINITE};
use aricode_virtio::blk;

/// How long a request may take before the device is considered not to
/// complete it (ms).
const REQUEST_TIMEOUT_MS: u64 = 2000;
/// Device resets in a row, with no request completed in between, after
/// which the device is given up (bounded recovery, ADR 0028).
const MAX_RESETS: u32 = 3;
/// Exit code when the device is given up after `MAX_RESETS` resets.
const EXIT_GIVEN_UP: u64 = 50;
/// Exit code when the features change across a reset.
const EXIT_FEATURES_CHANGED: u64 = 51;
/// Exit code of a `strict` halt.
const EXIT_STRICT: u64 = 52;

fn say(msg: &[u8]) {
    sys::print(b"virtio-blk: ");
    sys::print(msg);
    sys::print(b"\n");
}

/// A fault event in the kernel ring (ADR 0028; `diag` lists it).
fn record(name: &str, code: u64) {
    let _ = sys::event_record(code, name);
}

struct Device {
    transport: Transport,
    queue: Queue,
    /// Own DMA buffer: header (16), one data sector (512) and status (1).
    req: DmaRegion,
    /// `VIRTIO_BLK_F_FLUSH` negotiated: `T_FLUSH` makes writes durable.
    flush: bool,
    /// `blk_fault=LBA` (tests): the first client request that covers it
    /// is not notified to the device (a lost doorbell).
    fault_lba: Option<u64>,
    /// The request being issued is the injected one: no notification.
    lose_notify: bool,
    /// `strict` (ADR 0028): the first failed request halts the driver.
    strict: bool,
    /// Client buffers of requests the device did not complete, kept until
    /// it is reset (incident 0017). One at most: the device is reset before
    /// such a request is answered.
    owned: DeviceOwned<1>,
    /// Device resets since the last request the device completed.
    resets: u32,
}

fn setup(mask: u64) -> Result<Device, u64> {
    let transport =
        Transport::open(mask, blk::F_FLUSH, blk::CFG_CAPACITY + 8).map_err(|e| e.code())?;
    let flush = transport.features & blk::F_FLUSH != 0;
    let queue = transport.setup_queue(0, 64).map_err(|e| e.code())?;
    transport.driver_ok();
    let req = DmaRegion::alloc(4096).map_err(|_| 30u64)?;
    Ok(Device {
        transport,
        queue,
        req,
        flush,
        fault_lba: None,
        lose_notify: false,
        strict: false,
        owned: DeviceOwned::new(),
        resets: 0,
    })
}

impl Device {
    fn capacity(&self) -> u64 {
        self.transport.device_cfg.r64(blk::CFG_CAPACITY)
    }

    /// One synchronous request: header and status in the own buffer, data in
    /// `(data_phys, data_len)` if any (`T_FLUSH` has none); waits for the
    /// MSI-X interrupt and checks the status. On a failure the device may
    /// still own the request's buffers (`device_owns_buffers`); with
    /// `strict` the driver halts there.
    fn request(
        &mut self,
        header: blk::RequestHeader,
        data: Option<(u64, u32, bool)>,
    ) -> Result<(), u64> {
        let result = self.issue(header, data);
        if let (Err(code), true) = (result, self.strict) {
            self.halt(code);
        }
        result
    }

    /// `strict`: prints the failure and the state it left and exits,
    /// without resetting the device and with every buffer it may own kept
    /// (the client's handle is still open); the kernel clears bus mastering
    /// before anything is freed (ADR 0038) and init restarts nothing.
    fn halt(&self, code: u64) -> ! {
        let mut buf = [0u8; 20];
        sys::print(b"virtio-blk: strict: halting on a failed request (code ");
        sys::print(aricode_rt::fmt_u64(code, &mut buf));
        sys::print(b"); descriptors in flight ");
        sys::print(aricode_rt::fmt_u64(
            self.queue.vq.in_flight() as u64,
            &mut buf,
        ));
        sys::print(b", device status ");
        sys::print(aricode_rt::fmt_u64(
            self.transport
                .common
                .r8(aricode_virtio::common::DEVICE_STATUS) as u64,
            &mut buf,
        ));
        sys::print(b"\n");
        record("blk strict halt", code);
        sys::process_exit(EXIT_STRICT)
    }

    fn issue(
        &mut self,
        header: blk::RequestHeader,
        data: Option<(u64, u32, bool)>,
    ) -> Result<(), u64> {
        self.req
            .slice_mut(0, 16)
            .copy_from_slice(&header.to_bytes());
        self.req.slice_mut(528, 1)[0] = 0xFF;
        let status = (self.req.phys + 528, 1, true);
        let head = match data {
            Some((phys, len, device_writes)) => self.queue.vq.add(&[
                (self.req.phys, 16, false),
                (phys, len, device_writes),
                status,
            ]),
            None => self.queue.vq.add(&[(self.req.phys, 16, false), status]),
        }
        .ok_or(40u64)?;
        if core::mem::take(&mut self.lose_notify) {
            say(b"injected fault: the device is not notified of this request (blk_fault)");
        } else {
            self.queue.notify(&self.transport);
        }
        loop {
            match self.queue.vq.pop_used() {
                Ok(Some((done, _))) => {
                    if done != head {
                        return Err(41);
                    }
                    self.resets = 0;
                    break;
                }
                Ok(None) => {}
                // The device returned what it never held (audit
                // 2026-10-25 A, M1): the request fails and the device is
                // reset, as for a timeout.
                Err(_) => return Err(45),
            }
            match sys::irq_wait(IRQ, REQUEST_TIMEOUT_MS) {
                Ok(_) => {}
                Err(Error::Timeout) => return Err(42),
                Err(_) => return Err(43),
            }
        }
        match self.req.slice_mut(528, 1)[0] {
            blk::STATUS_OK => Ok(()),
            _ => Err(44),
        }
    }

    /// The device may still read or write a buffer it was given: a request
    /// it did not complete left descriptors in the ring, or it asks to be
    /// reset (`DEVICE_NEEDS_RESET`, virtio 1.2, 2.1.2), after which it
    /// completes nothing.
    fn device_owns_buffers(&self) -> bool {
        self.queue.vq.in_flight() > 0 || self.transport.needs_reset()
    }

    /// Hands back the handle of the client buffer of the request just
    /// served: closed when the device owns nothing or was never given it
    /// (`given` false: a request without data, or one refused before the
    /// device), otherwise kept until the device is reset (`recover`), so
    /// that the buffer cannot be freed and reused under a device that may
    /// still write into it.
    fn settle_client_buffer(&mut self, buffer: Handle, given: bool, lba: u64) {
        if !given || !self.device_owns_buffers() {
            let _ = sys::handle_close(buffer);
            return;
        }
        match self.owned.keep(buffer) {
            Kept::Held => {
                say(b"client buffer kept until the device is reset: its request did not complete")
            }
            Kept::ForGood => say(b"client buffer kept for good: no room to track it"),
        }
        record("blk buf kept", lba);
    }

    /// After a request the device did not complete: resets the device
    /// (virtio 1.2, 2.4), negotiates the same features, creates the queue
    /// again on its memory and sets `DRIVER_OK`; then the client buffers
    /// kept for that request are released. Exits when the reset fails, when
    /// the features change, or after `MAX_RESETS` resets in a row.
    fn recover(&mut self) {
        self.resets += 1;
        say(b"resetting the device: a request did not complete");
        record("blk reset", self.resets as u64);
        if let Err(code) = self.restart() {
            self.give_up(b"the device does not come back from its reset", code);
        }
        say(b"device reset done: queue 0 created again, DRIVER_OK set");
        let released = self.owned.release();
        if released > 0 {
            say(b"client buffer released: the device was reset");
            record("blk buf released", released as u64);
        }
        if self.resets >= MAX_RESETS {
            self.give_up(
                b"no request completed across the last resets",
                EXIT_GIVEN_UP,
            );
        }
    }

    fn restart(&mut self) -> Result<(), u64> {
        let before = self.transport.features;
        self.transport.reset().map_err(|e| e.code())?;
        self.transport
            .negotiate(blk::F_FLUSH)
            .map_err(|e| e.code())?;
        // The capacity and `flush` were read with these features.
        if self.transport.features != before {
            return Err(EXIT_FEATURES_CHANGED);
        }
        self.transport
            .restart_queue(&mut self.queue)
            .map_err(|e| e.code())?;
        self.transport.driver_ok();
        Ok(())
    }

    /// Bounded recovery exhausted: exits. The kernel clears the device's
    /// bus mastering before it frees anything this driver held, kept
    /// client buffers included (ADR 0038), and the supervisor restarts the
    /// driver, which starts with its own reset.
    fn give_up(&self, why: &[u8], code: u64) -> ! {
        sys::print(b"virtio-blk: device given up: ");
        sys::print(why);
        sys::print(b"; exiting\n");
        record("blk given up", code);
        sys::process_exit(code)
    }

    /// Reads one sector into the own buffer (for the GPT).
    fn read_own(&mut self, lba: u64) -> Result<&[u8], u64> {
        self.request(
            blk::RequestHeader::read(lba),
            Some((self.req.phys + 16, 512, true)),
        )?;
        Ok(self.req.slice_mut(16, 512))
    }
}

/// Reads the GPT and announces the partitions.
fn report_partitions(dev: &mut Device) {
    let mut buf = [0u8; 20];
    let header = match dev
        .read_own(1)
        .ok()
        .and_then(|s| aricode_gpt::Header::parse(s).ok())
    {
        Some(h) => h,
        None => {
            say(b"no GPT");
            return;
        }
    };
    // The table (16 KiB) goes on the heap: the user stack is small.
    let mut table = alloc::vec![0u8; 128 * aricode_gpt::ENTRY_LEN];
    let sectors = header.entries_bytes() / 512;
    for i in 0..sectors.min(32) {
        match dev.read_own(header.entries_lba + i as u64) {
            Ok(s) => table[i * 512..(i + 1) * 512].copy_from_slice(s),
            Err(_) => {
                say(b"error reading the GPT table");
                return;
            }
        }
    }
    let mut parts = [None; 8];
    match aricode_gpt::partitions(&header, &table, &mut parts) {
        Ok(n) => {
            sys::print(b"virtio-blk: GPT with ");
            sys::print(aricode_rt::fmt_u64(n as u64, &mut buf));
            sys::print(b" partition(s)\n");
            for p in parts.iter().flatten() {
                sys::print(b"virtio-blk:   ");
                sys::print(p.name_str().as_bytes());
                sys::print(b" LBA ");
                sys::print(aricode_rt::fmt_u64(p.first_lba, &mut buf));
                sys::print(b"..");
                sys::print(aricode_rt::fmt_u64(p.last_lba, &mut buf));
                sys::print(b"\n");
            }
        }
        Err(_) => say(b"corrupt GPT table"),
    }
}

/// Service loop: one request per message, reply through the endpoint
/// transferred in the message.
fn serve(dev: &mut Device) -> ! {
    let capacity = dev.capacity();
    let mut msg = [0u8; 32];
    // LBA of the request being served, for the fault events.
    let mut lba_served = 0;
    let mut handles = [Handle(0); 2];
    let mut buffers = ClientBuffers::new();
    say(b"serving requests");
    loop {
        let (n, nh) = match sys::ipc_receive(SERVICE, &mut msg, &mut handles, TIMEOUT_INFINITE) {
            Ok(r) => r,
            // The service endpoint was closed or revoked (ADR 0030): nobody
            // can send here again, so polling it would spin.
            Err(e) if e.endpoint_gone() => {
                say(b"service endpoint closed; exiting");
                sys::process_exit(9);
            }
            Err(_) => continue,
        };
        if nh == 0 {
            continue; // without a reply endpoint there is no one to answer
        }
        let reply_to = handles[0];
        // Whether the request's buffer was handed to the device.
        let mut given = false;
        let reply = match Request::decode(&msg[..n]) {
            None => Reply {
                status: Status::Invalid,
                value: 0,
            },
            Some(Request { op: Op::Info, .. }) => Reply {
                status: Status::Ok,
                value: capacity,
            },
            Some(Request { op: Op::Flush, .. }) => {
                let done = if dev.flush {
                    dev.request(blk::RequestHeader::flush(), None)
                } else {
                    Ok(())
                };
                Reply {
                    status: if done.is_ok() { Status::Ok } else { Status::Io },
                    value: 0,
                }
            }
            Some(Request { op, count, lba }) => {
                lba_served = lba;
                let count = count.min(MAX_SECTORS) as u64;
                if nh < 2 {
                    Reply {
                        status: Status::BadHandles,
                        value: 0,
                    }
                } else if count == 0 || lba.checked_add(count).is_none_or(|end| end > capacity) {
                    Reply {
                        status: Status::Invalid,
                        value: 0,
                    }
                } else {
                    match map_client_buffer(&mut buffers, handles[1]) {
                        Some((phys, len)) if len >= count * 512 => {
                            let bytes = (count * 512) as u32;
                            given = true;
                            if dev
                                .fault_lba
                                .is_some_and(|f| (lba..lba + count).contains(&f))
                            {
                                dev.fault_lba = None;
                                dev.lose_notify = true;
                            }
                            let r = match op {
                                Op::Read => dev.request(
                                    blk::RequestHeader::read(lba),
                                    Some((phys, bytes, true)),
                                ),
                                Op::Write => dev.request(
                                    blk::RequestHeader::write(lba),
                                    Some((phys, bytes, false)),
                                ),
                                Op::Info | Op::Flush => unreachable!(),
                            };
                            match r {
                                Ok(()) => Reply {
                                    status: Status::Ok,
                                    value: count,
                                },
                                Err(_) => Reply {
                                    status: Status::Io,
                                    value: 0,
                                },
                            }
                        }
                        _ => Reply {
                            status: Status::BadHandles,
                            value: 0,
                        },
                    }
                }
            }
        };
        if nh >= 2 {
            dev.settle_client_buffer(handles[1], given, lba_served);
        }
        // A request the device may still complete: it is reset before the
        // client hears back, so no late write lands in a buffer the client
        // has reused for its next request (audit 2026-10-25 C, L7); only
        // then is a kept buffer released.
        if dev.device_owns_buffers() {
            dev.recover();
        }
        let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
        let _ = sys::handle_close(reply_to);
    }
}

fn main(mask: u64) -> u64 {
    let mut dev = match setup(mask) {
        Ok(d) => d,
        Err(step) => {
            say(b"initialization failed");
            return step;
        }
    };
    let mut args = [0u8; sys::CMDLINE_MAX];
    let line = sys::cmdline(extra_handle(mask, 0), &mut args);
    if line.is_err() {
        say(b"cannot read the command line (too long or not text): boot flags ignored");
        record("blk cmdline unreadable", sys::CMDLINE_MAX as u64);
    }
    if let Ok(text) = line {
        let c = aricode_cmdline::CmdLine::new(text);
        dev.fault_lba = c
            .get(aricode_cmdline::keys::BLK_FAULT)
            .and_then(|v| v.parse().ok());
        dev.strict = c.has(aricode_cmdline::keys::STRICT);
        if dev.strict {
            say(b"strict mode: the first failed request halts the driver");
        }
        if dev.fault_lba.is_some() {
            say(b"blk_fault: the first request on the given LBA will not be notified");
        }
    }
    let mut buf = [0u8; 20];
    say(b"device ready");
    say(if dev.flush {
        b"flush negotiated (VIRTIO_BLK_F_FLUSH)"
    } else {
        b"no flush: the device does not offer it"
    });
    sys::print(b"virtio-blk: capacity ");
    sys::print(aricode_rt::fmt_u64(dev.capacity(), &mut buf));
    sys::print(b" sectors\n");
    report_partitions(&mut dev);
    if dev.device_owns_buffers() {
        dev.recover();
    }
    serve(&mut dev)
}

aricode_rt::entry!(main);
