//! Driver de prueba del IOMMU (Fase 33): recibe el virtio-blk con el contrato
//! de `virtio_pci` (SERVICE = handle SYSTEM con READ), lee el sector 0 en su
//! propio buffer DMA (traducido por la unidad) y despues pide al dispositivo
//! escribir en una direccion fisica que no le pertenece: la peticion no debe
//! completarse y la unidad debe haber registrado el fallo.
#![no_std]
#![no_main]

use aricode_rt::fmt_u64;
use aricode_rt::sys::virtio_pci::{DmaRegion, Queue, Transport, IRQ, SERVICE};
use aricode_rt::sys::{self, abi, Error};
use aricode_virtio::blk;

/// Memoria baja reservada: nunca es de este proceso ni de nadie.
const FOREIGN_PHYS: u64 = 0x1000;

fn submit(
    transport: &Transport,
    queue: &mut Queue,
    req: &mut DmaRegion,
    data_phys: u64,
    timeout_ms: u64,
) -> Result<bool, u64> {
    req.slice_mut(0, 16)
        .copy_from_slice(&blk::RequestHeader::read(0).to_bytes());
    req.slice_mut(528, 1)[0] = 0xFF;
    let head = queue
        .vq
        .add(&[
            (req.phys, 16, false),
            (data_phys, 512, true),
            (req.phys + 528, 1, true),
        ])
        .ok_or(40u64)?;
    queue.notify(transport);
    loop {
        if let Some((done, _)) = queue.vq.pop_used() {
            return if done == head { Ok(true) } else { Err(41) };
        }
        match sys::irq_wait(IRQ, timeout_ms) {
            Ok(_) => {}
            Err(Error::Timeout) => return Ok(false),
            Err(_) => return Err(43),
        }
    }
}

fn faults() -> Result<u64, u64> {
    let mut buf = [0u8; 24];
    if sys::system_info(SERVICE, abi::sysinfo::IOMMU, &mut buf) != Ok(24) {
        return Err(50);
    }
    Ok(u64::from_le_bytes(buf[16..24].try_into().unwrap()))
}

fn main(mask: u64) -> u64 {
    let transport = match Transport::open(mask, 0) {
        Ok(t) => t,
        Err(e) => return e.code(),
    };
    let mut queue = match transport.setup_queue(0, 8) {
        Ok(q) => q,
        Err(e) => return e.code(),
    };
    transport.driver_ok();
    let Ok(mut req) = DmaRegion::alloc(4096) else {
        return 30;
    };
    let Ok(before) = faults() else {
        return 50;
    };

    // 1. Lectura legitima: el sector 0 (MBR protector de la GPT) en el buffer propio.
    let own_data = req.phys + 16;
    match submit(&transport, &mut queue, &mut req, own_data, 2000) {
        Ok(true) => {}
        Ok(false) => return 20,
        Err(e) => return e,
    }
    if req.slice_mut(528, 1)[0] != blk::STATUS_OK {
        return 21;
    }
    let sector = req.slice_mut(16, 512);
    if sector[510] != 0x55 || sector[511] != 0xAA {
        return 22;
    }

    // 2. DMA a memoria ajena. Lo que el dispositivo cuente de la peticion
    // (no completarla, error de E/S o incluso "exito", como hace QEMU al
    // descartar la escritura) no prueba nada: la prueba es que la unidad
    // registro el fallo y, del lado del kernel, que esa memoria no cambio.
    match submit(&transport, &mut queue, &mut req, FOREIGN_PHYS, 500) {
        Ok(false) => sys::print(b"dma_rogue: la peticion ilegal no se completo\n"),
        Ok(true) if req.slice_mut(528, 1)[0] != blk::STATUS_OK => {
            sys::print(b"dma_rogue: el dispositivo informo de error de E/S\n")
        }
        Ok(true) => sys::print(b"dma_rogue: el dispositivo dio la peticion por completada\n"),
        Err(e) => return e,
    }
    let Ok(after) = faults() else {
        return 50;
    };
    if after <= before {
        sys::print(b"dma_rogue: la unidad no registro el fallo\n");
        return 24;
    }
    let mut b = [0u8; 20];
    sys::print(b"dma_rogue: lectura legitima ok, DMA a memoria ajena bloqueado (");
    sys::print(fmt_u64(after - before, &mut b));
    sys::print(b" fallo(s) registrados)\n");
    0
}

aricode_rt::entry!(main);
