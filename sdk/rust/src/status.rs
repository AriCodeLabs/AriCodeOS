//! Cliente del estado de servicios (`aricode-proto::status`): pregunta a
//! init, por el endpoint del broker, por cada servicio supervisado.

use crate::{rpc, Error, Handle};
use aricode_proto::status::Request;
pub use aricode_proto::status::{LastExit, Reply, State};

/// Estado del servicio `index`; `Ok(None)` cuando no hay más. Cada petición
/// usa un endpoint de respuesta propio y esperas acotadas: una respuesta
/// tardía de otra petición nunca se confunde con esta.
pub fn service_status(broker: Handle, index: u8) -> Result<Option<Reply>, Error> {
    let mut msg = [0u8; Reply::LEN];
    let len = rpc::call(
        broker,
        &Request { index }.encode(),
        &[],
        rpc::GRACE_MS,
        rpc::GRACE_MS,
        &mut msg,
    )?;
    let reply = Reply::decode(&msg[..len]).ok_or(Error::Invalid)?;
    Ok(if reply.found { Some(reply) } else { None })
}
