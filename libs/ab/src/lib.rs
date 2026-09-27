//! Actualización A/B (Fase 30). Tres piezas puras, probadas en host y usadas
//! por AriBoot, init, el actualizador y xtask:
//!
//! - [`BootState`]: el sector 0 de la partición `ARIBOOT` (slot activo, slot
//!   pendiente con contador de intentos, CRC).
//! - [`Image`]: una imagen de sistema en una partición de slot: cabecera
//!   `ARISLOT` con la longitud, y a continuación un contenedor `aricode-initrd`
//!   con `manifest`, `manifest.sig`, `kernel.elf` e `initrd.img`. El
//!   manifiesto lleva la versión y los SHA-256 de kernel e initrd; la firma
//!   Ed25519 cubre el manifiesto entero.
//! - [`choose`]/[`confirm`]: la política de arranque: un slot pendiente se
//!   intenta mientras le queden intentos; si se agotan o su imagen no
//!   verifica, se vuelve al activo (rollback); el sistema arrancado confirma
//!   y el pendiente pasa a activo.

#![no_std]

use aricode_crypto::{ed25519, sha256, unhex};
use aricode_initrd::Initrd;

pub const STATE_MAGIC: &[u8; 8] = b"ARIBOOT\0";
pub const IMAGE_MAGIC: &[u8; 8] = b"ARISLOT\0";
pub const SECTOR: usize = 512;
/// Intentos que recibe un slot recién instalado.
pub const DEFAULT_TRIES: u8 = 3;
/// Nombres de partición que busca el cargador.
pub const PART_STATE: &str = "ARIBOOT";
pub const PART_SLOT_A: &str = "SYSA";
pub const PART_SLOT_B: &str = "SYSB";
pub const PART_UPDATE: &str = "UPDATE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Slot::A => "A",
            Slot::B => "B",
        }
    }
    pub fn partition(self) -> &'static str {
        match self {
            Slot::A => PART_SLOT_A,
            Slot::B => PART_SLOT_B,
        }
    }
    fn code(self) -> u8 {
        match self {
            Slot::A => 0,
            Slot::B => 1,
        }
    }
    fn from_code(c: u8) -> Option<Slot> {
        match c {
            0 => Some(Slot::A),
            1 => Some(Slot::B),
            _ => None,
        }
    }
}

/// Estado de arranque (sector 0 de `ARIBOOT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootState {
    pub active: Slot,
    /// Slot instalado a la espera de confirmación, con sus intentos restantes.
    pub pending: Option<(Slot, u8)>,
    /// Arranques contados por el cargador.
    pub boot_count: u32,
}

impl BootState {
    pub const fn fresh(active: Slot) -> Self {
        Self {
            active,
            pending: None,
            boot_count: 0,
        }
    }

    pub fn encode(&self) -> [u8; SECTOR] {
        let mut s = [0u8; SECTOR];
        s[..8].copy_from_slice(STATE_MAGIC);
        s[8] = 1; // versión del formato
        s[9] = self.active.code();
        match self.pending {
            Some((slot, tries)) => {
                s[10] = slot.code();
                s[11] = tries;
            }
            None => {
                s[10] = 0xFF;
                s[11] = 0;
            }
        }
        s[12..16].copy_from_slice(&self.boot_count.to_le_bytes());
        let crc = aricode_crypto::sha256::sha256(&s[..60]);
        s[60..64].copy_from_slice(&crc[..4]);
        s
    }

    /// `None` si el sector no es un estado válido (magia, versión o CRC).
    pub fn decode(s: &[u8]) -> Option<Self> {
        if s.len() < 64 || &s[..8] != STATE_MAGIC || s[8] != 1 {
            return None;
        }
        let crc = aricode_crypto::sha256::sha256(&s[..60]);
        if s[60..64] != crc[..4] {
            return None;
        }
        let active = Slot::from_code(s[9])?;
        let pending = if s[10] == 0xFF {
            None
        } else {
            Some((Slot::from_code(s[10])?, s[11]))
        };
        Some(Self {
            active,
            pending,
            boot_count: u32::from_le_bytes([s[12], s[13], s[14], s[15]]),
        })
    }
}

/// Qué slot arrancar y qué estado escribir antes de hacerlo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub slot: Slot,
    /// `true` si el slot arrancado espera confirmación.
    pub pending: bool,
    /// Estado a escribir en `ARIBOOT` antes de saltar al kernel.
    pub state: BootState,
    /// Motivo legible (para el log del cargador).
    pub reason: &'static str,
}

/// Política de arranque: consume un intento del slot pendiente o vuelve al
/// activo cuando se agotan.
pub fn choose(state: BootState) -> Decision {
    let mut next = state;
    next.boot_count = state.boot_count.wrapping_add(1);
    match state.pending {
        Some((slot, tries)) if tries > 0 => {
            next.pending = Some((slot, tries - 1));
            Decision {
                slot,
                pending: true,
                state: next,
                reason: "slot pendiente con intentos",
            }
        }
        Some((_, _)) => {
            next.pending = None;
            Decision {
                slot: state.active,
                pending: false,
                state: next,
                reason: "slot pendiente agotado: rollback al activo",
            }
        }
        None => Decision {
            slot: state.active,
            pending: false,
            state: next,
            reason: "slot activo",
        },
    }
}

/// Si la imagen del slot elegido no verifica, el cargador descarta el
/// pendiente y vuelve al activo.
pub fn reject(state: BootState) -> Decision {
    let mut next = state;
    next.pending = None;
    Decision {
        slot: state.active,
        pending: false,
        state: next,
        reason: "imagen del slot pendiente rechazada: rollback al activo",
    }
}

/// El sistema arrancado desde `slot` funciona: pasa a activo.
pub fn confirm(state: BootState, slot: Slot) -> BootState {
    BootState {
        active: slot,
        pending: None,
        boot_count: state.boot_count,
    }
}

/// Estado tras instalar una imagen nueva en el slot inactivo.
pub fn schedule(state: BootState, slot: Slot) -> BootState {
    BootState {
        active: state.active,
        pending: Some((slot, DEFAULT_TRIES)),
        boot_count: state.boot_count,
    }
}

// ---------------------------------------------------------------- imágenes

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    BadHeader,
    BadContainer,
    MissingFile(&'static str),
    BadManifest,
    BadSignature,
    HashMismatch(&'static str),
}

/// Manifiesto analizado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    pub kernel_sha256: [u8; 32],
    pub initrd_sha256: [u8; 32],
}

impl Manifest {
    pub fn parse(text: &str) -> Option<Self> {
        let (mut version, mut kernel, mut initrd) = (None, None, None);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line.split_once('=')?;
            let v = v.trim();
            match k.trim() {
                "version" => version = Some(v.parse().ok()?),
                "kernel" => {
                    let mut h = [0u8; 32];
                    unhex(v, &mut h)?;
                    kernel = Some(h);
                }
                "initrd" => {
                    let mut h = [0u8; 32];
                    unhex(v, &mut h)?;
                    initrd = Some(h);
                }
                _ => return None,
            }
        }
        Some(Self {
            version: version?,
            kernel_sha256: kernel?,
            initrd_sha256: initrd?,
        })
    }

    /// Texto canónico del manifiesto (lo que se firma).
    pub fn write<'a>(&self, out: &'a mut [u8; 192]) -> &'a str {
        let mut n = 0;
        let mut put = |s: &[u8], n: &mut usize| {
            out[*n..*n + s.len()].copy_from_slice(s);
            *n += s.len();
        };
        put(b"version = ", &mut n);
        let mut num = [0u8; 10];
        let mut v = self.version;
        let mut i = num.len();
        loop {
            i -= 1;
            num[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        put(&num[i..], &mut n);
        put(b"\nkernel = ", &mut n);
        let mut hex = [0u8; 64];
        put(
            aricode_crypto::hex(&self.kernel_sha256, &mut hex).as_bytes(),
            &mut n,
        );
        put(b"\ninitrd = ", &mut n);
        put(
            aricode_crypto::hex(&self.initrd_sha256, &mut hex).as_bytes(),
            &mut n,
        );
        put(b"\n", &mut n);
        core::str::from_utf8(&out[..n]).unwrap_or("")
    }
}

/// Imagen verificada: referencias a los bytes dentro del contenedor.
#[derive(Debug, Clone, Copy)]
pub struct Verified<'a> {
    pub manifest: Manifest,
    pub kernel: &'a [u8],
    pub initrd: &'a [u8],
}

pub struct Image;

impl Image {
    /// Cabecera de un slot: magia y longitud del contenedor que sigue.
    pub fn header(container_len: u32) -> [u8; SECTOR] {
        let mut h = [0u8; SECTOR];
        h[..8].copy_from_slice(IMAGE_MAGIC);
        h[8..12].copy_from_slice(&container_len.to_le_bytes());
        h
    }

    /// Longitud del contenedor según la cabecera del slot.
    pub fn container_len(header: &[u8]) -> Result<u32, ImageError> {
        if header.len() < 12 || &header[..8] != IMAGE_MAGIC {
            return Err(ImageError::BadHeader);
        }
        let len = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
        if len == 0 {
            return Err(ImageError::BadHeader);
        }
        Ok(len)
    }

    /// Verifica un contenedor: firma del manifiesto con `pubkey` y hashes de
    /// kernel e initrd. Solo devuelve datos si todo cuadra.
    pub fn verify<'a>(container: &'a [u8], pubkey: &[u8; 32]) -> Result<Verified<'a>, ImageError> {
        let rd = Initrd::parse(container).map_err(|_| ImageError::BadContainer)?;
        let manifest_bytes = rd
            .find("manifest")
            .ok_or(ImageError::MissingFile("manifest"))?;
        let sig = rd
            .find("manifest.sig")
            .ok_or(ImageError::MissingFile("manifest.sig"))?;
        let kernel = rd
            .find("kernel.elf")
            .ok_or(ImageError::MissingFile("kernel.elf"))?;
        let initrd = rd
            .find("initrd.img")
            .ok_or(ImageError::MissingFile("initrd.img"))?;
        if sig.len() != ed25519::SIGNATURE_LEN {
            return Err(ImageError::BadSignature);
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(sig);
        if !ed25519::verify(pubkey, manifest_bytes, &signature) {
            return Err(ImageError::BadSignature);
        }
        let text = core::str::from_utf8(manifest_bytes).map_err(|_| ImageError::BadManifest)?;
        let manifest = Manifest::parse(text).ok_or(ImageError::BadManifest)?;
        if sha256::sha256(kernel) != manifest.kernel_sha256 {
            return Err(ImageError::HashMismatch("kernel.elf"));
        }
        if sha256::sha256(initrd) != manifest.initrd_sha256 {
            return Err(ImageError::HashMismatch("initrd.img"));
        }
        Ok(Verified {
            manifest,
            kernel,
            initrd,
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    #[test]
    fn state_round_trip_and_crc() {
        let s = BootState {
            active: Slot::A,
            pending: Some((Slot::B, 2)),
            boot_count: 41,
        };
        let sector = s.encode();
        assert_eq!(BootState::decode(&sector), Some(s));
        let mut bad = sector;
        bad[11] = 3;
        assert_eq!(BootState::decode(&bad), None, "CRC");
        assert_eq!(BootState::decode(&[0u8; 512]), None);
        assert_eq!(
            BootState::decode(&BootState::fresh(Slot::B).encode())
                .unwrap()
                .active,
            Slot::B
        );
    }

    #[test]
    fn boot_policy() {
        let fresh = BootState::fresh(Slot::A);
        let d = choose(fresh);
        assert_eq!((d.slot, d.pending, d.state.boot_count), (Slot::A, false, 1));
        // Instalar B: tres intentos, luego rollback.
        let mut st = schedule(fresh, Slot::B);
        assert_eq!(st.pending, Some((Slot::B, 3)));
        for tries_left in [2u8, 1, 0] {
            let d = choose(st);
            assert_eq!((d.slot, d.pending), (Slot::B, true));
            assert_eq!(d.state.pending, Some((Slot::B, tries_left)));
            st = d.state;
        }
        let d = choose(st);
        assert_eq!((d.slot, d.pending), (Slot::A, false), "agotado");
        assert_eq!(d.state.pending, None);
        // Confirmacion tras el primer arranque bueno.
        let d = choose(schedule(fresh, Slot::B));
        let confirmed = confirm(d.state, d.slot);
        assert_eq!((confirmed.active, confirmed.pending), (Slot::B, None));
        // Imagen rechazada: rollback inmediato.
        let r = reject(schedule(fresh, Slot::B));
        assert_eq!((r.slot, r.state.pending), (Slot::A, None));
    }

    fn build(seed: &[u8; 32], version: u32, kernel: &[u8], initrd: &[u8]) -> Vec<u8> {
        let m = Manifest {
            version,
            kernel_sha256: sha256::sha256(kernel),
            initrd_sha256: sha256::sha256(initrd),
        };
        let mut buf = [0u8; 192];
        let text = m.write(&mut buf).as_bytes().to_vec();
        let sig = ed25519::sign(seed, &text);
        aricode_initrd::build(&[
            ("manifest", text.as_slice()),
            ("manifest.sig", &sig[..]),
            ("kernel.elf", kernel),
            ("initrd.img", initrd),
        ])
        .unwrap()
    }

    #[test]
    fn manifest_text_round_trip() {
        let m = Manifest {
            version: 12,
            kernel_sha256: [0xAB; 32],
            initrd_sha256: [0x01; 32],
        };
        let mut buf = [0u8; 192];
        let text = m.write(&mut buf);
        assert!(text.starts_with("version = 12\nkernel = abab"));
        assert_eq!(Manifest::parse(text), Some(m));
        assert!(Manifest::parse("version = x").is_none());
        assert!(Manifest::parse("version = 1\nkernel = 00\ninitrd = 00").is_none());
    }

    #[test]
    fn image_verification() {
        let seed = [9u8; 32];
        let pk = ed25519::public_key(&seed);
        let kernel = b"ELF falso pero consistente".to_vec();
        let initrd = b"ARIRD falso".to_vec();
        let img = build(&seed, 7, &kernel, &initrd);
        let v = Image::verify(&img, &pk).unwrap();
        assert_eq!(v.manifest.version, 7);
        assert_eq!(v.kernel, kernel.as_slice());
        assert_eq!(v.initrd, initrd.as_slice());
        // Otra clave.
        let other = ed25519::public_key(&[1u8; 32]);
        assert_eq!(
            Image::verify(&img, &other).err(),
            Some(ImageError::BadSignature)
        );
        // Kernel alterado dentro del contenedor: hash distinto.
        let mut tampered = img.clone();
        let pos = tampered
            .windows(kernel.len())
            .position(|w| w == kernel.as_slice())
            .unwrap();
        tampered[pos] ^= 1;
        assert_eq!(
            Image::verify(&tampered, &pk).err(),
            Some(ImageError::HashMismatch("kernel.elf"))
        );
        // Contenedor sin firma.
        let unsigned =
            aricode_initrd::build(&[("manifest", &b"version = 1"[..]), ("kernel.elf", &b"k"[..])])
                .unwrap();
        assert_eq!(
            Image::verify(&unsigned, &pk).err(),
            Some(ImageError::MissingFile("manifest.sig"))
        );
        // Cabecera de slot.
        let h = Image::header(img.len() as u32);
        assert_eq!(Image::container_len(&h), Ok(img.len() as u32));
        assert_eq!(
            Image::container_len(&[0u8; 512]),
            Err(ImageError::BadHeader)
        );
    }
}
