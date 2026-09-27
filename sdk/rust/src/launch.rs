//! Cliente del broker de lanzamiento (`aricode-proto::launch`): pedir a init
//! que ejecute un programa del initrd con las capabilities de su manifiesto.

use crate::{self as sys, rights, Error, Handle, TIMEOUT_INFINITE};
pub use aricode_proto::launch::Status;
use aricode_proto::launch::{Reply, Request};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchError {
    Ipc(Error),
    Status(Status),
    /// Nombre vacío o demasiado largo.
    BadName,
}

/// Ejecuta `name` con `arg` a través del broker y devuelve su código de salida.
pub fn run(broker: Handle, name: &str, arg: u64) -> Result<u64, LaunchError> {
    let req = Request::new(name, arg).ok_or(LaunchError::BadName)?;
    let reply_ep = sys::endpoint_create().map_err(LaunchError::Ipc)?;
    let reply_w = sys::handle_duplicate(reply_ep, rights::WRITE | rights::TRANSFER)
        .map_err(LaunchError::Ipc)?;
    sys::ipc_send(broker, &req.encode(), &[reply_w], TIMEOUT_INFINITE).map_err(LaunchError::Ipc)?;
    let mut msg = [0u8; Reply::LEN];
    let mut none: [Handle; 0] = [];
    let result = sys::ipc_receive(reply_ep, &mut msg, &mut none, TIMEOUT_INFINITE);
    let _ = sys::handle_close(reply_ep);
    let (len, _) = result.map_err(LaunchError::Ipc)?;
    let reply = Reply::decode(&msg[..len]).ok_or(LaunchError::Ipc(Error::Invalid))?;
    match reply.status {
        Status::Ok => Ok(reply.value),
        s => Err(LaunchError::Status(s)),
    }
}
