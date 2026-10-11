//! Input service client (`aricode-proto::input`).

use crate::{rpc, Error, Handle};
pub use aricode_proto::input::Event;
use aricode_proto::input::{Op, Reply, Request, Status, MAX_EVENTS};

pub struct InputClient {
    service: Handle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    Ipc(Error),
    Status(Status),
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
        let n = r.count as usize;
        out[..n].copy_from_slice(&r.events[..n]);
        times[..n].copy_from_slice(&r.times[..n]);
        Ok(n)
    }
}
