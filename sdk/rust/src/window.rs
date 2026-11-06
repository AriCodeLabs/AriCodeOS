// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Compositor client (`aricode-proto::window`): a window with its
//! content DMA buffer and its events.

use crate::{self as sys, rights, rpc, Error, Handle};
use aricode_gfx::Canvas;
pub use aricode_proto::window::Event;
pub use aricode_proto::window::{mods, wants, DamageRect, KeyState};
use aricode_proto::window::{Op, Reply, Request, Status, MAX_DAMAGE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowError {
    Ipc(Error),
    Status(Status),
    /// Title too long or invalid size.
    Invalid,
}

pub struct Window {
    service: Handle,
    id: u32,
    dma: Handle,
    pixels: u64,
    width: u32,
    height: u32,
}

impl Window {
    /// Creates the window: allocates the content buffer with `hardware` and
    /// transfers it to the compositor along with the request.
    pub fn create(
        service: Handle,
        hardware: Handle,
        width: u32,
        height: u32,
        title: &str,
    ) -> Result<Self, WindowError> {
        Self::create_wanting(service, hardware, width, height, title, 0)
    }

    /// As [`create`](Self::create), asking for the events in `events`
    /// ([`wants`]).
    pub fn create_wanting(
        service: Handle,
        hardware: Handle,
        width: u32,
        height: u32,
        title: &str,
        events: u32,
    ) -> Result<Self, WindowError> {
        if width == 0 || height == 0 {
            return Err(WindowError::Invalid);
        }
        let len = width as u64 * height as u64 * 4;
        let dma = sys::dma_alloc(hardware, len).map_err(WindowError::Ipc)?;
        let pixels = sys::mmio_map(dma).map_err(WindowError::Ipc)?;
        let mut req = Request::new(Op::Create)
            .with_title(title)
            .ok_or(WindowError::Invalid)?;
        req.width = width;
        req.height = height;
        req.events = events;
        let mut w = Self {
            service,
            id: 0,
            dma,
            pixels,
            width,
            height,
        };
        w.canvas().fill(0);
        let r = w.call(req, true)?;
        w.id = r.value as u32;
        Ok(w)
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    /// Canvas over the content buffer.
    pub fn canvas(&mut self) -> Canvas<'_> {
        let n = (self.width * self.height) as usize;
        // SAFETY: our own DMA buffer of `width*height*4` bytes, mapped RW.
        let pixels = unsafe { core::slice::from_raw_parts_mut(self.pixels as *mut u32, n) };
        Canvas::new(pixels, self.width, self.height, self.width).expect("window buffer")
    }

    fn call(&self, req: Request, with_buffer: bool) -> Result<Reply, WindowError> {
        let mut extra = [Handle(0); 1];
        let mut n = 0;
        if with_buffer {
            // `mmio_map` requires READ|WRITE even though the compositor only reads.
            extra[0] =
                sys::handle_duplicate(self.dma, rights::READ | rights::WRITE | rights::TRANSFER)
                    .map_err(WindowError::Ipc)?;
            n = 1;
        }
        let mut msg = [0u8; Reply::LEN];
        let wait = req.timeout_ms as u64 + rpc::GRACE_MS;
        let len = rpc::call(
            self.service,
            &req.encode(),
            &extra[..n],
            rpc::GRACE_MS,
            wait,
            &mut msg,
        )
        .map_err(WindowError::Ipc)?;
        let reply = Reply::decode(&msg[..len]).ok_or(WindowError::Ipc(Error::Invalid))?;
        match reply.status {
            Status::Ok => Ok(reply),
            s => Err(WindowError::Status(s)),
        }
    }

    fn request(&self, op: Op) -> Request {
        let mut r = Request::new(op);
        r.window = self.id;
        r
    }

    /// Asks the compositor to repaint the content: what was painted since
    /// the last `present` reaches the screen with its next frame (ADR 0046).
    pub fn present(&self) -> Result<(), WindowError> {
        self.call(self.request(Op::Present), false).map(|_| ())
    }

    /// As [`present`](Self::present), for what changed: at most
    /// [`MAX_DAMAGE`] rectangles reach the compositor; more mean all of it.
    pub fn present_rects(&self, rects: &[DamageRect]) -> Result<(), WindowError> {
        let mut req = self.request(Op::Present);
        if rects.len() <= MAX_DAMAGE {
            req.damage[..rects.len()].copy_from_slice(rects);
            req.damage_len = rects.len() as u8;
        }
        self.call(req, false).map(|_| ())
    }

    /// Gives the content a new size, in a new buffer, blank until painted. The
    /// old buffer goes once the compositor lets it go: for this call, the
    /// program's quota must hold both.
    pub fn resize(&mut self, hardware: Handle, width: u32, height: u32) -> Result<(), WindowError> {
        if width == 0 || height == 0 {
            return Err(WindowError::Invalid);
        }
        let dma =
            sys::dma_alloc(hardware, width as u64 * height as u64 * 4).map_err(WindowError::Ipc)?;
        let pixels = match sys::mmio_map(dma) {
            Ok(p) => p,
            Err(e) => {
                let _ = sys::handle_close(dma);
                return Err(WindowError::Ipc(e));
            }
        };
        let old = (self.dma, self.pixels, self.width, self.height);
        (self.dma, self.pixels, self.width, self.height) = (dma, pixels, width, height);
        self.canvas().fill(0);
        let mut req = self.request(Op::Resize);
        req.width = width;
        req.height = height;
        match self.call(req, true) {
            Ok(_) => {
                let _ = sys::mmio_unmap(old.1);
                let _ = sys::handle_close(old.0);
                Ok(())
            }
            Err(e) => {
                let _ = sys::mmio_unmap(pixels);
                let _ = sys::handle_close(dma);
                (self.dma, self.pixels, self.width, self.height) = old;
                Err(e)
            }
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Next event, or `None` if the deadline expires.
    pub fn poll(&self, timeout_ms: u32) -> Result<Option<Event>, WindowError> {
        let mut req = self.request(Op::Poll);
        req.timeout_ms = timeout_ms;
        match self.call(req, false) {
            Ok(r) => Ok(r.event),
            Err(WindowError::Status(Status::Timeout)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Starts a `poll` without waiting for its answer, for a program that
    /// also waits on other endpoints (the terminal: its shell's output):
    /// returns the endpoint the answer comes on (receive side), to wait on
    /// with `ipc_receive_any` and read with [`poll_reply`]. The caller
    /// closes it once answered.
    pub fn start_poll(&self, timeout_ms: u32) -> Result<Handle, WindowError> {
        let mut req = self.request(Op::Poll);
        req.timeout_ms = timeout_ms;
        let (reply, reply_to) = sys::endpoint_create_pair().map_err(WindowError::Ipc)?;
        if let Err(e) = sys::ipc_send(self.service, &req.encode(), &[reply_to], rpc::GRACE_MS) {
            // Not transferred: both still ours.
            let _ = sys::handle_close(reply_to);
            let _ = sys::handle_close(reply);
            return Err(WindowError::Ipc(e));
        }
        Ok(reply)
    }

    pub fn close(self) -> Result<(), WindowError> {
        self.call(self.request(Op::Close), false).map(|_| ())
    }
}

/// The answer to [`Window::start_poll`]: the event, or `None` if its
/// deadline expired.
pub fn poll_reply(msg: &[u8]) -> Result<Option<Event>, WindowError> {
    let reply = Reply::decode(msg).ok_or(WindowError::Ipc(Error::Invalid))?;
    match reply.status {
        Status::Ok => Ok(reply.event),
        Status::Timeout => Ok(None),
        s => Err(WindowError::Status(s)),
    }
}

/// Broker and init only, over the compositor's main endpoint: a session
/// for one client (ADR 0052); returns its send side (`WRITE`,
/// transferable) and its id for [`detach`]. Windows created through it are
/// that client's alone.
pub fn attach(compositor: Handle) -> Result<(Handle, u32), WindowError> {
    let req = Request::new(Op::Attach);
    let mut msg = [0u8; Reply::LEN];
    let mut handles = [Handle(0); sys::abi::IPC_MAX_HANDLES];
    let (len, nh) = rpc::call_handles(
        compositor,
        &req.encode(),
        &[],
        rpc::GRACE_MS,
        rpc::GRACE_MS,
        &mut msg,
        &mut handles,
    )
    .map_err(WindowError::Ipc)?;
    let reply = Reply::decode(&msg[..len]);
    let session = match reply.map(|r| (r.status, r.value)) {
        Some((Status::Ok, id)) if nh == 1 => return Ok((handles[0], id as u32)),
        Some((Status::Ok, _)) | None => WindowError::Ipc(Error::Invalid),
        Some((s, _)) => WindowError::Status(s),
    };
    for h in &handles[..nh] {
        let _ = sys::handle_close(*h);
    }
    Err(session)
}

/// Broker and init only: the client of session `id` has exited, so its
/// windows close now (ADR 0052).
pub fn detach(compositor: Handle, id: u32) -> Result<(), WindowError> {
    let mut req = Request::new(Op::Detach);
    req.window = id;
    let mut msg = [0u8; Reply::LEN];
    let len = rpc::call(
        compositor,
        &req.encode(),
        &[],
        rpc::GRACE_MS,
        rpc::GRACE_MS,
        &mut msg,
    )
    .map_err(WindowError::Ipc)?;
    match Reply::decode(&msg[..len]).map(|r| r.status) {
        Some(Status::Ok) => Ok(()),
        Some(s) => Err(WindowError::Status(s)),
        None => Err(WindowError::Ipc(Error::Invalid)),
    }
}
