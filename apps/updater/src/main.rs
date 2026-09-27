//! `updater` (Fase 30): lee la imagen de la partición `UPDATE`, verifica su
//! firma y sus hashes con la clave pública de desarrollo, la copia al slot
//! inactivo y deja ese slot pendiente con tres intentos. AriBoot lo probará
//! en el siguiente arranque e init lo confirmará si el sistema levanta.
#![no_std]
#![no_main]

extern crate alloc;

use aricode_ab::{schedule, BootState, Image, Slot, PART_STATE, PART_UPDATE};
use aricode_rt::fmt_u64;
use aricode_rt::sys::block::BlockClient;
use aricode_rt::sys::{self, kind, permissions};

const PUBKEY_HEX: &str = include_str!("../../../keys/dev/ed25519.pub");
/// Buffer DMA donde se carga y verifica la imagen entera.
const IMAGE_BUFFER: u64 = 4 * 1024 * 1024;
const CHUNK: u16 = 16;

fn say(msg: &[u8]) {
    sys::print(b"updater: ");
    sys::print(msg);
    sys::print(b"\n");
}

fn num(v: u64) {
    let mut b = [0u8; 20];
    sys::print(fmt_u64(v, &mut b));
}

fn find_partition(blk: &mut BlockClient, name: &str) -> Option<(u64, u64)> {
    blk.read(1, 1).ok()?;
    let header = aricode_gpt::Header::parse(&blk.buffer()[..512]).ok()?;
    let sectors = header.entries_bytes().div_ceil(512) as u64;
    let mut table = alloc::vec![0u8; sectors as usize * 512];
    let mut lba = 0;
    while lba < sectors {
        let n = (sectors - lba).min(CHUNK as u64) as u16;
        blk.read(header.entries_lba + lba, n).ok()?;
        let o = lba as usize * 512;
        table[o..o + n as usize * 512].copy_from_slice(&blk.buffer()[..n as usize * 512]);
        lba += n as u64;
    }
    let mut parts = [None; 16];
    let count = aricode_gpt::partitions(&header, &table, &mut parts).ok()?;
    parts[..count]
        .iter()
        .flatten()
        .find(|p| p.name_str() == name)
        .map(|p| (p.first_lba, p.last_lba))
}

/// Copia `sectors` sectores desde `src` a `dst` a través del buffer del cliente.
fn copy_sectors(blk: &mut BlockClient, src: u64, dst: u64, sectors: u64) -> bool {
    let mut done = 0;
    while done < sectors {
        let n = (sectors - done).min(CHUNK as u64) as u16;
        if blk.read(src + done, n).is_err() || blk.write(dst + done, n).is_err() {
            return false;
        }
        done += n as u64;
    }
    true
}

fn main(_arg: u64) -> u64 {
    let (mut block, mut hardware) = (None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) {
            block = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hardware = Some(h);
        }
    });
    let (Some(block), Some(hw)) = (block, hardware) else {
        say(b"necesito el servicio de bloques (capacidad block)");
        return 1;
    };
    let Ok(mut blk) = BlockClient::new(block, hw, CHUNK) else {
        return 2;
    };
    let mut pk = [0u8; 32];
    if aricode_crypto::unhex(PUBKEY_HEX.trim(), &mut pk).is_none() {
        return 3;
    }
    // 1. Particiones.
    let Some(update) = find_partition(&mut blk, PART_UPDATE) else {
        say(b"sin particion UPDATE");
        return 4;
    };
    let Some(state_part) = find_partition(&mut blk, PART_STATE) else {
        say(b"sin particion ARIBOOT");
        return 5;
    };
    if blk.read(state_part.0, 1).is_err() {
        return 6;
    }
    let state = BootState::decode(&blk.buffer()[..512]).unwrap_or(BootState::fresh(Slot::A));
    let target = state.active.other();
    let Some(slot_part) = find_partition(&mut blk, target.partition()) else {
        say(b"sin particion para el slot inactivo");
        return 7;
    };
    // 2. Cargar la imagen de UPDATE en un buffer DMA y verificarla.
    if blk.read(update.0, 1).is_err() {
        return 8;
    }
    let Ok(len) = Image::container_len(&blk.buffer()[..512]) else {
        say(b"UPDATE no contiene una imagen de sistema");
        return 9;
    };
    let sectors = (len as u64).div_ceil(512);
    if (sectors + 1) * 512 > IMAGE_BUFFER || sectors + 1 > slot_part.1 - slot_part.0 + 1 {
        say(b"la imagen no cabe");
        return 10;
    }
    let Ok(dma) = sys::dma_alloc(hw, IMAGE_BUFFER) else {
        say(b"sin memoria para la imagen (cuota)");
        return 11;
    };
    let Ok(virt) = sys::mmio_map(dma) else {
        return 11;
    };
    // SAFETY: buffer DMA propio de IMAGE_BUFFER bytes, mapeado RW.
    let image = unsafe { core::slice::from_raw_parts_mut(virt as *mut u8, IMAGE_BUFFER as usize) };
    let mut done = 0u64;
    while done < sectors {
        let n = (sectors - done).min(CHUNK as u64) as u16;
        if blk.read(update.0 + 1 + done, n).is_err() {
            return 12;
        }
        let o = done as usize * 512;
        image[o..o + n as usize * 512].copy_from_slice(&blk.buffer()[..n as usize * 512]);
        done += n as u64;
    }
    let version = match Image::verify(&image[..len as usize], &pk) {
        Ok(v) => v.manifest.version,
        Err(_) => {
            say(b"la imagen de UPDATE no verifica (firma o hashes); no se instala");
            return 13;
        }
    };
    sys::print(b"updater: imagen en UPDATE version ");
    num(version as u64);
    sys::print(b" verificada (");
    num(len as u64);
    sys::print(b" bytes)\n");
    // 3. Copiar cabecera + contenedor al slot inactivo (sector a sector, del
    //    disco al disco; la copia en memoria ya sirvio para verificar).
    if !copy_sectors(&mut blk, update.0, slot_part.0, sectors + 1) {
        say(b"error escribiendo el slot");
        return 14;
    }
    sys::print(b"updater: instalada en el slot ");
    sys::print(target.name().as_bytes());
    sys::print(b"\n");
    // 4. Programar el slot: pendiente con intentos.
    let next = schedule(state, target);
    blk.buffer()[..512].copy_from_slice(&next.encode());
    if blk.write(state_part.0, 1).is_err() {
        say(b"error escribiendo el estado de arranque");
        return 15;
    }
    if blk.read(state_part.0, 1).is_err() {
        return 16;
    }
    match BootState::decode(&blk.buffer()[..512]) {
        Some(s) if s.pending == Some((target, aricode_ab::DEFAULT_TRIES)) => {
            sys::print(b"updater: estado: activo ");
            sys::print(s.active.name().as_bytes());
            sys::print(b", pendiente ");
            sys::print(target.name().as_bytes());
            sys::print(b" (");
            num(aricode_ab::DEFAULT_TRIES as u64);
            sys::print(b" intentos); reinicia para probarlo\n");
            0
        }
        _ => {
            say(b"el estado releido no coincide");
            17
        }
    }
}

aricode_rt::entry!(main);
