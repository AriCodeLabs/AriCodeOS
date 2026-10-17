// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! A/B boot (Phase 30): looks on the GPT disks for the `ARIBOOT` state
//! partition and the `SYSA`/`SYSB` slots, applies the `aricode-ab` policy
//! (pending slot attempts, rollback), verifies the manifest's Ed25519
//! signature and the kernel and initrd hashes, and returns the chosen image.
//! If there is no state or no image verifies, the caller boots from the ESP
//! as usual (recovery).

use alloc::vec::Vec;
use aricode_ab::{choose, reject, unpersisted, BootState, Decision, Image, ImageError, Slot};
use aricode_gpt::{partitions, Header, Partition};
use log::{info, warn};
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::media::block::BlockIO;

const SECTOR: usize = 512;
/// Chosen and verified system image.
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

    /// GPT partitions of the disk, if it is one.
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

/// Reads and verifies a slot image. Returns kernel, initrd and version.
fn load_slot(
    disk: &mut Disk,
    part: &Partition,
    pk: &[u8; 32],
) -> Result<(Vec<u8>, Vec<u8>, u32), &'static str> {
    let header = disk
        .read(part.first_lba, 1)
        .ok_or("could not read the slot")?;
    let len = Image::container_len(&header).map_err(|_| "invalid slot header")? as usize;
    let sectors = len.div_ceil(SECTOR);
    if sectors as u64 + 1 > part.sectors() {
        return Err("image larger than the partition");
    }
    let mut container = disk
        .read(part.first_lba + 1, sectors)
        .ok_or("could not read the image")?;
    container.truncate(len);
    let verified = Image::verify(&container, pk).map_err(|e| match e {
        ImageError::BadSignature => "invalid signature",
        ImageError::HashMismatch(_) => "kernel or initrd hash mismatch",
        ImageError::BadManifest => "invalid manifest",
        _ => "malformed image",
    })?;
    Ok((
        verified.kernel.to_vec(),
        verified.initrd.to_vec(),
        verified.manifest.version,
    ))
}

/// Tries to boot from the slots. `None` = boot from the ESP.
pub fn load() -> Option<Loaded> {
    let Some(pk) = aricode_crypto::trust::public_key() else {
        warn!("slots: invalid development public key");
        return None;
    };
    let handles = boot::find_handles::<BlockIO>().ok()?;
    for handle in handles {
        // Non-exclusive open: an exclusive open disconnects the drivers using
        // the disk (the ESP's FAT file system among them) and afterwards the
        // ESP can no longer be read.
        // SAFETY: AriBoot is the only running application; it only reads
        // sectors and writes the state sector, which no other driver uses.
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
                warn!("slots: invalid boot state, assuming A active");
                BootState::fresh(Slot::A)
            });
        match state.pending {
            Some((slot, tries)) => info!(
                "slots: state active {}, pending {} ({} tries)",
                state.active.name(),
                slot.name(),
                tries
            ),
            None => info!("slots: state active {}, no pending", state.active.name()),
        }
        let mut decision: Decision = choose(state);
        info!(
            "slots: {} -> slot {}",
            decision.reason,
            decision.slot.name()
        );
        for attempt in 0..2 {
            let Some(part) = find(&parts, decision.slot.partition()) else {
                warn!("slots: missing partition {}", decision.slot.partition());
                break;
            };
            match load_slot(&mut disk, part, &pk) {
                Ok((kernel, initrd, version)) => {
                    if !disk.write(state_lba, &decision.state.encode()) {
                        warn!("slots: could not write the boot state");
                        if decision.pending && attempt == 0 {
                            // A pending slot without a counted try could boot
                            // forever: fall back to the active one, which needs none.
                            decision = unpersisted(state);
                            info!(
                                "slots: {} -> slot {}",
                                decision.reason,
                                decision.slot.name()
                            );
                            continue;
                        }
                    }
                    info!(
                        "slots: booting from slot {} (image version {}{})",
                        decision.slot.name(),
                        version,
                        if decision.pending {
                            ", pending confirmation"
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
                    warn!("slots: slot {} rejected: {why}", decision.slot.name());
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
        warn!("slots: no valid image; booting from the ESP");
        return None;
    }
    info!("slots: no ARIBOOT partition, booting from the ESP");
    None
}
