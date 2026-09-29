//! Paquetes de aplicación firmados (etapa 4, ADR 0017). Un paquete es un
//! contenedor `aricode-initrd` con cuatro ficheros:
//!
//! ```text
//! manifest          name, version y SHA-256 de program y program.manifest
//! manifest.sig      firma Ed25519 del manifiesto entero
//! program           ELF estático de ring 3
//! program.manifest  manifiesto de aplicación (`aricode-manifest`)
//! ```
//!
//! El broker verifica el paquete al lanzar: firma con la clave pública de
//! desarrollo y hashes de ambos ficheros. Un paquete instalado es un solo
//! fichero de AriFS, así que instalarlo o actualizarlo es un `replace`
//! atómico. Mismo esquema que las imágenes de sistema (`aricode-ab`).

#![no_std]

#[cfg(feature = "write")]
extern crate std;

use aricode_crypto::{ed25519, sha256, unhex};
use aricode_initrd::Initrd;

pub const MAX_NAME: usize = 32;
/// Sufijo de los paquetes instalados: `apps/NOMBRE.aripkg`.
pub const EXTENSION: &str = ".aripkg";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgError {
    BadContainer,
    MissingFile(&'static str),
    BadManifest,
    BadSignature,
    HashMismatch(&'static str),
}

/// Manifiesto del paquete (lo que se firma).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PkgManifest {
    pub name: [u8; MAX_NAME],
    pub name_len: u8,
    pub version: u32,
    pub program_sha256: [u8; 32],
    pub manifest_sha256: [u8; 32],
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

impl PkgManifest {
    pub fn new(name: &str, version: u32, program: &[u8], manifest: &[u8]) -> Option<Self> {
        if !valid_name(name) {
            return None;
        }
        let mut n = [0u8; MAX_NAME];
        n[..name.len()].copy_from_slice(name.as_bytes());
        Some(Self {
            name: n,
            name_len: name.len() as u8,
            version,
            program_sha256: sha256::sha256(program),
            manifest_sha256: sha256::sha256(manifest),
        })
    }

    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
    }

    pub fn parse(text: &str) -> Option<Self> {
        let (mut name, mut version, mut program, mut manifest) = (None, None, None, None);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line.split_once('=')?;
            let v = v.trim();
            match k.trim() {
                "name" => {
                    if !valid_name(v) {
                        return None;
                    }
                    let mut n = [0u8; MAX_NAME];
                    n[..v.len()].copy_from_slice(v.as_bytes());
                    name = Some((n, v.len() as u8));
                }
                "version" => version = Some(v.parse().ok()?),
                "program" => {
                    let mut h = [0u8; 32];
                    unhex(v, &mut h)?;
                    program = Some(h);
                }
                "manifest" => {
                    let mut h = [0u8; 32];
                    unhex(v, &mut h)?;
                    manifest = Some(h);
                }
                _ => return None,
            }
        }
        let (name, name_len) = name?;
        Some(Self {
            name,
            name_len,
            version: version?,
            program_sha256: program?,
            manifest_sha256: manifest?,
        })
    }

    /// Texto canónico del manifiesto (lo que se firma).
    pub fn write<'a>(&self, out: &'a mut [u8; 256]) -> &'a str {
        let mut n = 0;
        let mut put = |s: &[u8], n: &mut usize| {
            out[*n..*n + s.len()].copy_from_slice(s);
            *n += s.len();
        };
        put(b"name = ", &mut n);
        put(&self.name[..self.name_len as usize], &mut n);
        put(b"\nversion = ", &mut n);
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
        let mut hex = [0u8; 64];
        put(b"\nprogram = ", &mut n);
        put(
            aricode_crypto::hex(&self.program_sha256, &mut hex).as_bytes(),
            &mut n,
        );
        put(b"\nmanifest = ", &mut n);
        put(
            aricode_crypto::hex(&self.manifest_sha256, &mut hex).as_bytes(),
            &mut n,
        );
        put(b"\n", &mut n);
        core::str::from_utf8(&out[..n]).unwrap_or("")
    }
}

/// Paquete verificado: referencias a los bytes dentro del contenedor.
#[derive(Debug, Clone, Copy)]
pub struct Verified<'a> {
    pub manifest: PkgManifest,
    pub program: &'a [u8],
    pub app_manifest: &'a [u8],
}

impl<'a> Verified<'a> {
    /// Desplazamiento del ELF dentro del contenedor (para lanzarlo desde el
    /// mismo buffer sin copiarlo).
    pub fn program_offset(&self, container: &[u8]) -> usize {
        self.program.as_ptr() as usize - container.as_ptr() as usize
    }
}

/// Verifica un paquete: firma del manifiesto con `pubkey` y hashes del
/// programa y de su manifiesto de aplicación. Solo devuelve datos si todo
/// cuadra.
pub fn verify<'a>(container: &'a [u8], pubkey: &[u8; 32]) -> Result<Verified<'a>, PkgError> {
    let rd = Initrd::parse(container).map_err(|_| PkgError::BadContainer)?;
    let manifest_bytes = rd
        .find("manifest")
        .ok_or(PkgError::MissingFile("manifest"))?;
    let sig = rd
        .find("manifest.sig")
        .ok_or(PkgError::MissingFile("manifest.sig"))?;
    let program = rd.find("program").ok_or(PkgError::MissingFile("program"))?;
    let app_manifest = rd
        .find("program.manifest")
        .ok_or(PkgError::MissingFile("program.manifest"))?;
    if sig.len() != ed25519::SIGNATURE_LEN {
        return Err(PkgError::BadSignature);
    }
    let mut signature = [0u8; 64];
    signature.copy_from_slice(sig);
    if !ed25519::verify(pubkey, manifest_bytes, &signature) {
        return Err(PkgError::BadSignature);
    }
    let text = core::str::from_utf8(manifest_bytes).map_err(|_| PkgError::BadManifest)?;
    let manifest = PkgManifest::parse(text).ok_or(PkgError::BadManifest)?;
    if sha256::sha256(program) != manifest.program_sha256 {
        return Err(PkgError::HashMismatch("program"));
    }
    if sha256::sha256(app_manifest) != manifest.manifest_sha256 {
        return Err(PkgError::HashMismatch("program.manifest"));
    }
    Ok(Verified {
        manifest,
        program,
        app_manifest,
    })
}

/// Construye y firma un paquete (herramientas de host). Con `corrupt` la
/// firma se altera para que no verifique.
#[cfg(feature = "write")]
pub fn build(
    name: &str,
    version: u32,
    program: &[u8],
    app_manifest: &[u8],
    seed: &[u8; 32],
    corrupt: bool,
) -> Option<std::vec::Vec<u8>> {
    let manifest = PkgManifest::new(name, version, program, app_manifest)?;
    let mut buf = [0u8; 256];
    let text = manifest.write(&mut buf).as_bytes().to_vec();
    let mut sig = ed25519::sign(seed, &text);
    if corrupt {
        sig[5] ^= 0x40;
    }
    aricode_initrd::build(&[
        ("manifest", text.as_slice()),
        ("manifest.sig", &sig[..]),
        ("program", program),
        ("program.manifest", app_manifest),
    ])
    .ok()
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    const SEED: [u8; 32] = [7u8; 32];

    #[test]
    fn build_verify_round_trip() {
        let pk = ed25519::public_key(&SEED);
        let elf = b"\x7fELF programa de mentira";
        let app = b"name = demo\ncaps = system\n";
        let pkg = build("demo", 3, elf, app, &SEED, false).unwrap();
        let v = verify(&pkg, &pk).unwrap();
        assert_eq!(v.manifest.name_str(), "demo");
        assert_eq!(v.manifest.version, 3);
        assert_eq!(v.program, elf);
        assert_eq!(v.app_manifest, app);
        assert_eq!(&pkg[v.program_offset(&pkg)..][..elf.len()], elf);
        let mut text = [0u8; 256];
        let canonical = v.manifest.write(&mut text);
        assert_eq!(PkgManifest::parse(canonical), Some(v.manifest));
    }

    #[test]
    fn rejects_tampering() {
        let pk = ed25519::public_key(&SEED);
        let elf = b"\x7fELF";
        let app = b"name = demo\n";
        assert_eq!(
            verify(&build("demo", 1, elf, app, &SEED, true).unwrap(), &pk).err(),
            Some(PkgError::BadSignature)
        );
        let other = ed25519::public_key(&[9u8; 32]);
        let pkg = build("demo", 1, elf, app, &SEED, false).unwrap();
        assert_eq!(verify(&pkg, &other).err(), Some(PkgError::BadSignature));
        // Programa cambiado tras firmar: el hash no cuadra.
        let mut tampered = pkg.clone();
        let off = verify(&pkg, &pk).unwrap().program_offset(&pkg);
        tampered[off + 1] ^= 0xFF;
        assert_eq!(
            verify(&tampered, &pk).err(),
            Some(PkgError::HashMismatch("program"))
        );
        assert_eq!(verify(b"basura", &pk).err(), Some(PkgError::BadContainer));
        assert!(build("con espacio", 1, elf, app, &SEED, false).is_none());
    }
}
