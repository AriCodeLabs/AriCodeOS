// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Service status client (`aricode-proto::status`): asks init, through the
//! broker endpoint, about each supervised service.

use crate::{rpc, Error, Handle};
use aricode_proto::status::Request;
pub use aricode_proto::status::{LastExit, Reply, State};

/// Status of service `index`; `Ok(None)` when there are no more. Each request
/// uses its own reply endpoint and bounded waits: a late reply to another
/// request is never mistaken for this one.
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
