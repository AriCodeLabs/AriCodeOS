// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Compositor client (`aricode-proto::window`): a window with its
//! content DMA buffer and its events.

use crate::{self as sys, rights, rpc, Error, Handle};
use aricode_gfx::Canvas;
pub use aricode_proto::window::Event;
use aricode_proto::window::{Op, Reply, Request, Status};

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

    pub fn close(self) -> Result<(), WindowError> {
        self.call(self.request(Op::Close), false).map(|_| ())
    }
}

/// Broker and init only, over the compositor's main endpoint: a session
/// for one client (ADR 0052); returns its send side (`WRITE`,
/// transferable). Windows created through it are that client's alone.
pub fn attach(compositor: Handle) -> Result<Handle, WindowError> {
    let req = Request::new(Op::Attach);
    let mut msg = [0u8; Reply::LEN];
    let mut handles = [Handle(0); 1];
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
    let reply = Reply::decode(&msg[..len]).ok_or(WindowError::Ipc(Error::Invalid))?;
    match (reply.status, nh) {
        (Status::Ok, 1) => Ok(handles[0]),
        (Status::Ok, _) => Err(WindowError::Ipc(Error::Invalid)),
        (s, _) => Err(WindowError::Status(s)),
    }
}
