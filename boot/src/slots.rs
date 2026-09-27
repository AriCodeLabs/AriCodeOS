//! Arranque A/B (Fase 30): busca en los discos GPT la partición de estado
//! `ARIBOOT` y los slots `SYSA`/`SYSB`, aplica la política de `aricode-ab`
//! (intentos del slot pendiente, rollback), verifica la firma Ed25519 del
//! manifiesto y los hashes de kernel e initrd, y devuelve la imagen elegida.
//! Si no hay estado o ninguna imagen verifica, el llamador arranca desde la
//! ESP como siempre (recuperación).

use alloc::vec::Vec;
use aricode_ab::{choose, reject, BootState, Decision, Image, ImageError, Slot};
use aricode_gpt::{partitions, Header, Partition};
use log::{info, warn};
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::media::block::BlockIO;

const SECTOR: usize = 512;
/// Clave pública que firma las imágenes (desarrollo; ver `keys/dev/README.md`).
const PUBKEY_HEX: &str = include_str!("../../keys/dev/ed25519.pub");

/// Imagen de sistema elegida y verificada.
pub struct Loaded {
    pub kernel: Vec<u8>,
    pub initrd: Vec<u8>,
    pub slot: Slot,
    pub pending: bool,
}

struct Disk {
    io: ScopedProtocol<BlockIO>,
    media_id: u32,
}

impl Disk {
    fn read(&mut self, lba: u64, sectors: usize) -> Option<Vec<u8>> {
        let mut buf = alloc::vec![0u8; sectors * SECTOR];
        self.io.read_blocks(self.media_id, lba, &mut buf).ok()?;
        Some(buf)
    }

    fn write(&mut self, lba: u64, data: &[u8; SECTOR]) -> bool {
        self.io.write_blocks(self.media_id, lba, data).is_ok() && self.io.flush_blocks().is_ok()
    }

    /// Particiones GPT del disco, si lo es.
    fn gpt(&mut self) -> Option<Vec<Partition>> {
        let sector = self.read(1, 1)?;
        let header = Header::parse(&sector).ok()?;
        let table = self.read(header.entries_lba, header.entries_bytes().div_ceil(SECTOR))?;
        let mut out = [None; 16];
        let n = partitions(&header, &table, &mut out).ok()?;
        Some(out.iter_mut().take(n).filter_map(Option::take).collect())
    }
}

fn find<'a>(parts: &'a [Partition], name: &str) -> Option<&'a Partition> {
    parts.iter().find(|p| p.name_str() == name)
}

fn pubkey() -> Option<[u8; 32]> {
    let mut pk = [0u8; 32];
    aricode_crypto::unhex(PUBKEY_HEX.trim(), &mut pk)?;
    Some(pk)
}

/// Lee y verifica la imagen de un slot. Devuelve kernel, initrd y versión.
fn load_slot(
    disk: &mut Disk,
    part: &Partition,
    pk: &[u8; 32],
) -> Result<(Vec<u8>, Vec<u8>, u32), &'static str> {
    let header = disk
        .read(part.first_lba, 1)
        .ok_or("no se pudo leer el slot")?;
    let len = Image::container_len(&header).map_err(|_| "cabecera de slot invalida")? as usize;
    let sectors = len.div_ceil(SECTOR);
    if sectors as u64 + 1 > part.sectors() {
        return Err("imagen mayor que la particion");
    }
    let mut container = disk
        .read(part.first_lba + 1, sectors)
        .ok_or("no se pudo leer la imagen")?;
    container.truncate(len);
    let verified = Image::verify(&container, pk).map_err(|e| match e {
        ImageError::BadSignature => "firma invalida",
        ImageError::HashMismatch(_) => "hash de kernel o initrd no coincide",
        ImageError::BadManifest => "manifiesto invalido",
        _ => "imagen malformada",
    })?;
    Ok((
        verified.kernel.to_vec(),
        verified.initrd.to_vec(),
        verified.manifest.version,
    ))
}

/// Intenta arrancar desde los slots. `None` = arrancar desde la ESP.
pub fn load() -> Option<Loaded> {
    let Some(pk) = pubkey() else {
        warn!("slots: clave publica de desarrollo invalida");
        return None;
    };
    let handles = boot::find_handles::<BlockIO>().ok()?;
    for handle in handles {
        // Apertura no exclusiva: una apertura exclusiva desconecta los drivers
        // que usan el disco (el sistema de ficheros FAT de la ESP entre ellos)
        // y despues no se puede leer la ESP.
        // SAFETY: AriBoot es la unica aplicacion en ejecucion; solo lee
        // sectores y escribe el sector de estado, que ningun otro driver usa.
        let Ok(io) = (unsafe {
            boot::open_protocol::<BlockIO>(
                OpenProtocolParams {
                    handle,
                    agent: boot::image_handle(),
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
        }) else {
            continue;
        };
        let media = io.media();
        if media.is_logical_partition()
            || media.block_size() as usize != SECTOR
            || !media.is_media_present()
        {
            continue;
        }
        let media_id = media.media_id();
        let mut disk = Disk { io, media_id };
        let Some(parts) = disk.gpt() else { continue };
        let Some(state_part) = find(&parts, aricode_ab::PART_STATE) else {
            continue;
        };
        let state_lba = state_part.first_lba;
        let state = disk
            .read(state_lba, 1)
            .and_then(|s| BootState::decode(&s))
            .unwrap_or_else(|| {
                warn!("slots: estado de arranque invalido, se asume A activo");
                BootState::fresh(Slot::A)
            });
        match state.pending {
            Some((slot, tries)) => info!(
                "slots: estado activo {}, pendiente {} ({} intentos)",
                state.active.name(),
                slot.name(),
                tries
            ),
            None => info!(
                "slots: estado activo {}, sin pendiente",
                state.active.name()
            ),
        }
        let mut decision: Decision = choose(state);
        info!(
            "slots: {} -> slot {}",
            decision.reason,
            decision.slot.name()
        );
        for attempt in 0..2 {
            let Some(part) = find(&parts, decision.slot.partition()) else {
                warn!("slots: falta la particion {}", decision.slot.partition());
                break;
            };
            match load_slot(&mut disk, part, &pk) {
                Ok((kernel, initrd, version)) => {
                    if !disk.write(state_lba, &decision.state.encode()) {
                        warn!("slots: no se pudo escribir el estado de arranque");
                    }
                    info!(
                        "slots: arrancando desde el slot {} (imagen version {}{})",
                        decision.slot.name(),
                        version,
                        if decision.pending {
                            ", pendiente de confirmacion"
                        } else {
                            ""
                        }
                    );
                    return Some(Loaded {
                        kernel,
                        initrd,
                        slot: decision.slot,
                        pending: decision.pending,
                    });
                }
                Err(why) => {
                    warn!("slots: slot {} rechazado: {why}", decision.slot.name());
                    if attempt == 0 && decision.slot != state.active {
                        decision = reject(state);
                        info!(
                            "slots: {} -> slot {}",
                            decision.reason,
                            decision.slot.name()
                        );
                        continue;
                    }
                    break;
                }
            }
        }
        warn!("slots: ninguna imagen valida; arranque desde la ESP");
        return None;
    }
    info!("slots: sin particion ARIBOOT, arranque desde la ESP");
    None
}
