//! Cliente del estado de servicios (`aricode-proto::status`): pregunta a
//! init, por el endpoint del broker, por cada servicio supervisado.

use crate::{self as sys, rights, Error, Handle};
use aricode_proto::status::Request;
pub use aricode_proto::status::{LastExit, Reply, State};

/// Estado del servicio `index`; `Ok(None)` cuando no hay más. Cada petición
/// usa un endpoint de respuesta propio y esperas acotadas: una respuesta
/// tardía de otra petición nunca se confunde con esta.
pub fn service_status(broker: Handle, index: u8) -> Result<Option<Reply>, Error> {
    const WAIT_MS: u64 = 5000;
    let reply_ep = sys::endpoint_create()?;
    let result = (|| {
        let reply_w = sys::handle_duplicate(reply_ep, rights::WRITE | rights::TRANSFER)?;
        sys::ipc_send(broker, &Request { index }.encode(), &[reply_w], WAIT_MS)?;
        let mut msg = [0u8; Reply::LEN];
        let mut none: [Handle; 0] = [];
        let (len, _) = sys::ipc_receive(reply_ep, &mut msg, &mut none, WAIT_MS)?;
        let reply = Reply::decode(&msg[..len]).ok_or(Error::Invalid)?;
        Ok(if reply.found { Some(reply) } else { None })
    })();
    let _ = sys::handle_close(reply_ep);
    result
}
