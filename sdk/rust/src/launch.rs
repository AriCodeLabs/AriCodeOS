//! Cliente del broker de lanzamiento (`aricode-proto::launch`): pedir a init
//! que ejecute un programa del initrd con las capabilities de su manifiesto.

use crate::{rpc, Error, Handle, TIMEOUT_INFINITE};
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
    // La respuesta es la salida del programa: la unica espera sin plazo.
    let mut msg = [0u8; Reply::LEN];
    let len = rpc::call(
        broker,
        &req.encode(),
        &[],
        rpc::GRACE_MS,
        TIMEOUT_INFINITE,
        &mut msg,
    )
    .map_err(LaunchError::Ipc)?;
    let reply = Reply::decode(&msg[..len]).ok_or(LaunchError::Ipc(Error::Invalid))?;
    match reply.status {
        Status::Ok => Ok(reply.value),
        s => Err(LaunchError::Status(s)),
    }
}
