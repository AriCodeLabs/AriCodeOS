//! Prueba de `ipc_receive_any` (etapa 5, ADR 0020): recibir del primero de
//! varios endpoints que tenga mensaje, con plazo; el indice devuelto dice
//! cual fue; sin mensajes vence el plazo; un handle que no es endpoint o sin
//! READ se rechaza antes de esperar.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, rights, Error, Handle};

fn main(_arg: u64) -> u64 {
    let Ok(a) = sys::endpoint_create() else {
        return 1;
    };
    let Ok(b) = sys::endpoint_create() else {
        return 1;
    };
    let Ok(c) = sys::endpoint_create() else {
        return 1;
    };
    let mut buf = [0u8; 16];
    let mut handles = [Handle(0); 4];
    // 1. Sin mensajes: plazo vencido.
    if sys::ipc_receive_any(&[a, b, c], &mut buf, &mut handles, 20) != Err(Error::Timeout) {
        sys::print(b"ipc_any: sin mensajes debia vencer el plazo\n");
        return 2;
    }
    // 2. Mensaje en el tercero: indice 2 y sus datos, con un handle transferido.
    let Ok(extra) = sys::endpoint_create() else {
        return 1;
    };
    let Ok(dup) = sys::handle_duplicate(extra, rights::WRITE | rights::TRANSFER) else {
        return 1;
    };
    if sys::ipc_send(c, b"tercero", &[dup], 0).is_err() {
        return 3;
    }
    match sys::ipc_receive_any(&[a, b, c], &mut buf, &mut handles, 1000) {
        Ok((2, n, 1)) if &buf[..n] == b"tercero" => {
            let _ = sys::handle_close(handles[0]);
        }
        other => {
            sys::print(b"ipc_any: resultado inesperado del tercero\n");
            let _ = other;
            return 4;
        }
    }
    // 3. Mensajes en dos a la vez: se sirven ambos, uno por llamada, en orden
    //    de indice.
    if sys::ipc_send(b, b"segundo", &[], 0).is_err()
        || sys::ipc_send(a, b"primero", &[], 0).is_err()
    {
        return 5;
    }
    let first = sys::ipc_receive_any(&[a, b, c], &mut buf, &mut handles, 1000);
    if !matches!(first, Ok((0, 7, 0))) || &buf[..7] != b"primero" {
        sys::print(b"ipc_any: el primero con mensaje debia ser el de indice 0\n");
        return 6;
    }
    let second = sys::ipc_receive_any(&[a, b, c], &mut buf, &mut handles, 1000);
    if !matches!(second, Ok((1, 7, 0))) || &buf[..7] != b"segundo" {
        return 7;
    }
    // 4. Un handle sin READ o que no es endpoint: Denied/Invalid sin esperar.
    let Ok(write_only) = sys::handle_duplicate(a, rights::WRITE) else {
        return 1;
    };
    if sys::ipc_receive_any(&[write_only, b], &mut buf, &mut handles, 1000) != Err(Error::Denied) {
        sys::print(b"ipc_any: un endpoint sin READ debia rechazarse\n");
        return 8;
    }
    if sys::ipc_receive_any(&[Handle::SELF_PROCESS], &mut buf, &mut handles, 1000)
        != Err(Error::Invalid)
    {
        return 9;
    }
    if sys::ipc_receive_any(&[], &mut buf, &mut handles, 10) != Err(Error::Invalid) {
        return 10;
    }
    sys::print(b"ipc_any: recepcion desde varios endpoints ok\n");
    0
}

aricode_rt::entry!(main);
