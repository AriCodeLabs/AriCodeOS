//! Llamada petición/respuesta sobre un endpoint de servicio (ADR 0012):
//! cada petición lleva su propio endpoint de respuesta, que se cierra al
//! terminar, y todas las esperas están acotadas. Una respuesta tardía llega
//! a un endpoint cerrado y se pierde: nunca se atribuye a otra petición.

use crate::{self as sys, rights, Error, Handle};

/// Handles extra que puede llevar una petición además del de respuesta.
pub const MAX_EXTRA: usize = 3;

/// Envía `request` con los handles `extra` (que se consumen: van al
/// servidor o se cierran si el envío falla) y espera la respuesta en `out`.
/// `send_wait_ms` acota la cola del servidor; `reply_wait_ms` la respuesta.
pub fn call(
    service: Handle,
    request: &[u8],
    extra: &[Handle],
    send_wait_ms: u64,
    reply_wait_ms: u64,
    out: &mut [u8],
) -> Result<usize, Error> {
    let mut none: [Handle; 0] = [];
    call_handles(
        service,
        request,
        extra,
        send_wait_ms,
        reply_wait_ms,
        out,
        &mut none,
    )
    .map(|(len, _)| len)
}

/// Como `call`, recogiendo además los handles que traiga la respuesta en
/// `handles_out`; devuelve `(bytes, handles)`.
pub fn call_handles(
    service: Handle,
    request: &[u8],
    extra: &[Handle],
    send_wait_ms: u64,
    reply_wait_ms: u64,
    out: &mut [u8],
    handles_out: &mut [Handle],
) -> Result<(usize, usize), Error> {
    if extra.len() > MAX_EXTRA {
        return Err(Error::Invalid);
    }
    let reply = sys::endpoint_create()?;
    let result = (|| {
        let reply_w = sys::handle_duplicate(reply, rights::WRITE | rights::TRANSFER)?;
        let mut handles = [Handle(0); MAX_EXTRA + 1];
        handles[0] = reply_w;
        handles[1..1 + extra.len()].copy_from_slice(extra);
        let sent = sys::ipc_send(service, request, &handles[..1 + extra.len()], send_wait_ms);
        if sent.is_err() {
            for h in &handles[..1 + extra.len()] {
                let _ = sys::handle_close(*h);
            }
        }
        sent?;
        sys::ipc_receive(reply, out, handles_out, reply_wait_ms)
    })();
    let _ = sys::handle_close(reply);
    result
}

/// Margen por encima del plazo que pide una petición: lo que se concede al
/// servicio para contestar cuando está vivo.
pub const GRACE_MS: u64 = 5000;
