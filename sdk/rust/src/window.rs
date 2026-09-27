//! Cliente del compositor (`aricode-proto::window`): una ventana con su
//! buffer DMA de contenido y sus eventos.

use crate::{self as sys, rights, Error, Handle, TIMEOUT_INFINITE};
use aricode_gfx::Canvas;
pub use aricode_proto::window::Event;
use aricode_proto::window::{Op, Reply, Request, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowError {
    Ipc(Error),
    Status(Status),
    /// Título demasiado largo o tamaño inválido.
    Invalid,
}

pub struct Window {
    service: Handle,
    reply: Handle,
    id: u32,
    dma: Handle,
    pixels: u64,
    width: u32,
    height: u32,
}

impl Window {
    /// Crea la ventana: reserva el buffer de contenido con `hardware` y lo
    /// transfiere al compositor junto con la petición.
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
        let reply = sys::endpoint_create().map_err(WindowError::Ipc)?;
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
            reply,
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

    /// Lienzo sobre el buffer de contenido.
    pub fn canvas(&mut self) -> Canvas<'_> {
        let n = (self.width * self.height) as usize;
        // SAFETY: buffer DMA propio de `width*height*4` bytes, mapeado RW.
        let pixels = unsafe { core::slice::from_raw_parts_mut(self.pixels as *mut u32, n) };
        Canvas::new(pixels, self.width, self.height, self.width).expect("buffer de la ventana")
    }

    fn call(&self, req: Request, with_buffer: bool) -> Result<Reply, WindowError> {
        let reply_w = sys::handle_duplicate(self.reply, rights::WRITE | rights::TRANSFER)
            .map_err(WindowError::Ipc)?;
        let mut handles = [reply_w, Handle(0)];
        let mut n = 1;
        if with_buffer {
            // `mmio_map` exige READ|WRITE aunque el compositor solo lea.
            handles[1] =
                sys::handle_duplicate(self.dma, rights::READ | rights::WRITE | rights::TRANSFER)
                    .map_err(WindowError::Ipc)?;
            n = 2;
        }
        sys::ipc_send(self.service, &req.encode(), &handles[..n], TIMEOUT_INFINITE)
            .map_err(WindowError::Ipc)?;
        let mut msg = [0u8; Reply::LEN];
        let mut none: [Handle; 0] = [];
        let (len, _) = sys::ipc_receive(self.reply, &mut msg, &mut none, TIMEOUT_INFINITE)
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

    /// Pide al compositor que vuelva a pintar el contenido.
    pub fn present(&self) -> Result<(), WindowError> {
        self.call(self.request(Op::Present), false).map(|_| ())
    }

    /// Siguiente evento, o `None` si vence el plazo.
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
