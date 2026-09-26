//! Cliente del servicio de bloques (Fase 19): info, lectura y escritura por
//! IPC con un buffer DMA propio. Handles: 0x10004 = endpoint del servicio
//! (WRITE), 0x10005 = HARDWARE (para el buffer DMA). Exito = 0.
#![no_std]
#![no_main]

use aricode_proto::block::{Op, Reply, Request, Status};
use user_testprogs::sys::{self, rights, Handle, TIMEOUT_INFINITE};

const BLK: Handle = Handle(0x0001_0004);
/// Primer sector del área en bruto del disco de pruebas (`xtask`).
const RAW: u64 = 4096;
const HW: Handle = Handle(0x0001_0005);

struct Client {
    reply: Handle,
    dma: Handle,
    buf: u64,
}

impl Client {
    fn call(&self, req: Request, with_buffer: bool) -> Option<Reply> {
        let reply_w = sys::handle_duplicate(self.reply, rights::WRITE | rights::TRANSFER).ok()?;
        let mut handles = [reply_w, Handle(0)];
        let mut n = 1;
        if with_buffer {
            handles[1] =
                sys::handle_duplicate(self.dma, rights::READ | rights::WRITE | rights::TRANSFER)
                    .ok()?;
            n = 2;
        }
        sys::ipc_send(BLK, &req.encode(), &handles[..n], TIMEOUT_INFINITE).ok()?;
        let mut msg = [0u8; 32];
        let mut none: [Handle; 0] = [];
        let (len, _) = sys::ipc_receive(self.reply, &mut msg, &mut none, 5000).ok()?;
        Reply::decode(&msg[..len])
    }

    fn sector(&mut self, i: usize) -> &mut [u8] {
        // SAFETY: buffer DMA de 8 KiB mapeado RW en este proceso.
        unsafe { core::slice::from_raw_parts_mut((self.buf + 512 * i as u64) as *mut u8, 512) }
    }
}

fn main(_arg: u64) -> u64 {
    let Ok(reply) = sys::endpoint_create() else {
        return 1;
    };
    let Ok(dma) = sys::dma_alloc(HW, 8192) else {
        return 2;
    };
    let Ok(buf) = sys::mmio_map(dma) else {
        return 3;
    };
    let mut c = Client { reply, dma, buf };
    // 1. Capacidad.
    let capacity = match c.call(
        Request {
            op: Op::Info,
            count: 0,
            lba: 0,
        },
        false,
    ) {
        Some(Reply {
            status: Status::Ok,
            value,
        }) if value > 0 => value,
        _ => return 4,
    };
    // 2. Leer 4 sectores del area en bruto (a partir del LBA 4096, tras el
    //    espacio reservado a AriFS) y comprobar el patron.
    match c.call(
        Request {
            op: Op::Read,
            count: 4,
            lba: RAW,
        },
        true,
    ) {
        Some(Reply {
            status: Status::Ok,
            value: 4,
        }) => {}
        _ => return 5,
    }
    for i in 0..4 {
        let s = c.sector(i);
        if &s[..8] != b"ARIDISK\0" || s[8..16] != (RAW + i as u64).to_le_bytes() {
            return 6;
        }
    }
    // 3. Escribir un patron en el LBA RAW + 100 y releerlo.
    for (i, b) in c.sector(0).iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(13).wrapping_add(1);
    }
    match c.call(
        Request {
            op: Op::Write,
            count: 1,
            lba: RAW + 100,
        },
        true,
    ) {
        Some(Reply {
            status: Status::Ok,
            value: 1,
        }) => {}
        _ => return 7,
    }
    c.sector(0).fill(0);
    match c.call(
        Request {
            op: Op::Read,
            count: 1,
            lba: RAW + 100,
        },
        true,
    ) {
        Some(Reply {
            status: Status::Ok,
            value: 1,
        }) => {}
        _ => return 8,
    }
    if c.sector(0)
        .iter()
        .enumerate()
        .any(|(i, b)| *b != (i as u8).wrapping_mul(13).wrapping_add(1))
    {
        return 9;
    }
    // 4. Fuera de rango y sin buffer: errores limpios.
    if c.call(
        Request {
            op: Op::Read,
            count: 1,
            lba: capacity,
        },
        true,
    )
    .map(|r| r.status)
        != Some(Status::Invalid)
    {
        return 10;
    }
    if c.call(
        Request {
            op: Op::Read,
            count: 1,
            lba: RAW,
        },
        false,
    )
    .map(|r| r.status)
        != Some(Status::BadHandles)
    {
        return 11;
    }
    sys::print(b"blk_client: info, lectura y escritura por IPC ok\n");
    0
}

user_testprogs::entry!(main);
