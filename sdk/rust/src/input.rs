//! Cliente del servicio de entrada (`aricode-proto::input`).

use crate::{self as sys, rights, Error, Handle};
pub use aricode_proto::input::Event;
use aricode_proto::input::{Op, Reply, Request, Status, MAX_EVENTS};

pub struct InputClient {
    service: Handle,
    reply: Handle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    Ipc(Error),
    Status(Status),
}

impl InputClient {
    pub fn new(service: Handle) -> Result<Self, Error> {
        Ok(Self {
            service,
            reply: sys::endpoint_create()?,
        })
    }

    /// Margen sobre el plazo pedido para que un servicio muerto no deje al
    /// cliente esperando para siempre (auditoría 4836023, hallazgo 1).
    const REPLY_GRACE_MS: u64 = 5000;

    fn call(&self, req: Request) -> Result<Reply, InputError> {
        let reply_w = sys::handle_duplicate(self.reply, rights::WRITE | rights::TRANSFER)
            .map_err(InputError::Ipc)?;
        sys::ipc_send(
            self.service,
            &req.encode(),
            &[reply_w],
            Self::REPLY_GRACE_MS,
        )
        .map_err(InputError::Ipc)?;
        let mut msg = [0u8; Reply::LEN];
        let mut none: [Handle; 0] = [];
        let wait = req.timeout_ms as u64 + Self::REPLY_GRACE_MS;
        let (len, _) =
            sys::ipc_receive(self.reply, &mut msg, &mut none, wait).map_err(InputError::Ipc)?;
        let reply = Reply::decode(&msg[..len]).ok_or(InputError::Ipc(Error::Invalid))?;
        match reply.status {
            Status::Ok => Ok(reply),
            s => Err(InputError::Status(s)),
        }
    }

    /// Nombre del dispositivo, copiado en `out`; devuelve su longitud.
    pub fn name(&self, out: &mut [u8; 32]) -> Result<usize, InputError> {
        let r = self.call(Request {
            op: Op::Info,
            timeout_ms: 0,
        })?;
        let n = r.name_len as usize;
        out[..n].copy_from_slice(&r.name[..n]);
        Ok(n)
    }

    /// Espera eventos hasta `timeout_ms`; devuelve cuántos hay en `out`, con
    /// el instante (ms de arranque) de cada uno en `times`.
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
        let n = r.count as usize;
        out[..n].copy_from_slice(&r.events[..n]);
        times[..n].copy_from_slice(&r.times[..n]);
        Ok(n)
    }
}
