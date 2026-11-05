// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Signed application packages (stage 4, ADR 0017). A package is an
//! `aricode-initrd` container with four files:
//!
//! ```text
//! manifest          name, version, SHA-256 of program and program.manifest,
//!                   and optionally `requires` (ADR 0074)
//! manifest.sig      Ed25519 signature of the whole manifest
//! program           static ring 3 ELF
//! program.manifest  application manifest (`aricode-manifest`)
//! ```
//!
//! The broker verifies the package at launch: signature with the development
//! public key and hashes of both files. An installed package is a single
//! AriFS file, so installing or updating it is an atomic
//! `replace`. Same scheme as the system images (`aricode-ab`).

#![no_std]

#[cfg(feature = "write")]
extern crate std;

use aricode_crypto::{ed25519, sha256, unhex};
use aricode_initrd::Initrd;

/// Largest package the system handles (incident 0026, ADR 0072 phase B):
/// eight times the largest program of 0.17 (the updater, 1 MiB). The
/// installer and the updater handle packages in pieces; the broker holds
/// the one it launches in a DMA buffer of its size, freed once the program
/// starts. AriFS v2 replaces up to `MAX_REPLACE` (16 MiB); a volume still
/// in v1 holds 32 KiB, and the installer says so.
pub const PACKAGE_MAX: usize = 8 * 1024 * 1024;
const _: () = assert!(PACKAGE_MAX <= aricode_proto::fs::MAX_REPLACE as usize);

pub const MAX_NAME: usize = 32;
/// Suffix of installed packages: `apps/NAME.aripkg`.
pub const EXTENSION: &str = ".aripkg";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgError {
    BadContainer,
    MissingFile(&'static str),
    BadManifest,
    BadSignature,
    HashMismatch(&'static str),
    /// The reader of a streaming verification failed.
    Unreadable,
}

/// Package manifest (what gets signed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PkgManifest {
    pub name: [u8; MAX_NAME],
    pub name_len: u8,
    pub version: u32,
    pub program_sha256: [u8; 32],
    pub manifest_sha256: [u8; 32],
    /// The lowest application interface level it runs on
    /// (`aricode_ab::APPS_LEVEL`, ADR 0074); 0 when the manifest has no
    /// `requires` line. A system older than ADR 0074 refuses a manifest
    /// that has one (its parser knows no such key), the safe failure.
    pub requires: u32,
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
            requires: 0,
        })
    }

    /// Whether a system offering application interface `level` runs it.
    pub fn runs_on(&self, level: u32) -> bool {
        self.requires <= level
    }

    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
    }

    pub fn parse(text: &str) -> Option<Self> {
        let (mut name, mut version, mut program, mut manifest) = (None, None, None, None);
        let mut requires = None;
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
                // One spelling: decimal digits without a sign or a leading
                // zero (so never `requires = 0`), and one line.
                "requires" if requires.is_none() => {
                    if v.starts_with('0') || !v.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    requires = Some(v.parse::<u32>().ok()?);
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
            requires: requires.unwrap_or(0),
        })
    }

    /// Canonical manifest text (what gets signed).
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
        put(decimal(self.version, &mut num), &mut n);
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
        // Written only when set, so every package without it keeps the
        // exact manifest (and signature) it always had.
        if self.requires > 0 {
            put(b"requires = ", &mut n);
            put(decimal(self.requires, &mut num), &mut n);
            put(b"\n", &mut n);
        }
        core::str::from_utf8(&out[..n]).unwrap_or("")
    }
}

/// `v` in decimal, at the end of `num`.
fn decimal(mut v: u32, num: &mut [u8; 10]) -> &[u8] {
    let mut i = num.len();
    loop {
        i -= 1;
        num[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    &num[i..]
}

/// Verified package: references to the bytes inside the container.
#[derive(Debug, Clone, Copy)]
pub struct Verified<'a> {
    pub manifest: PkgManifest,
    pub program: &'a [u8],
    pub app_manifest: &'a [u8],
}

impl<'a> Verified<'a> {
    /// Offset of the ELF inside the container (to launch it from the
    /// same buffer without copying it).
    pub fn program_offset(&self, container: &[u8]) -> usize {
        self.program.as_ptr() as usize - container.as_ptr() as usize
    }
}

/// Verifies a package: manifest signature with `pubkey` and hashes of the
/// program and of its application manifest. Only returns data if everything
/// matches.
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

/// Largest package manifest a streaming verification reads whole (a few
/// hundred bytes; the application manifest is hashed in pieces).
pub const SMALL_FILE_MAX: usize = 4096;

/// A package verified piece by piece: its manifest, and where the program
/// and its application manifest lie in it.
#[derive(Debug, Clone, Copy)]
pub struct StreamVerified {
    pub manifest: PkgManifest,
    pub program: aricode_initrd::Span,
    pub app_manifest: aricode_initrd::Span,
}

/// Bytes hashed per read of a streaming verification.
const HASH_PIECE: usize = 4096;

/// [`verify`] of a package of `len` bytes read piece by piece through
/// `read(offset, buf)`, for packages too large to hold (ADR 0072, phase
/// B): the same checks in the same order, and the same accepted packages,
/// except a package manifest over [`SMALL_FILE_MAX`], refused before its
/// signature is checked (project-signed manifests are a few hundred
/// bytes). A failed read is `Unreadable`.
pub fn verify_streaming(
    len: usize,
    mut read: impl FnMut(usize, &mut [u8]) -> Result<(), ()>,
    pubkey: &[u8; 32],
) -> Result<StreamVerified, PkgError> {
    let unreadable = core::cell::Cell::new(false);
    let mut fetch = |offset: usize, buf: &mut [u8]| {
        read(offset, buf).map_err(|()| {
            unreadable.set(true);
            aricode_initrd::InitrdError::Truncated
        })
    };
    let (parsed, program, app_manifest) = signed_head(len, &mut fetch, pubkey, &unreadable)?;
    let mut digest = |(offset, len): aricode_initrd::Span| {
        let mut h = sha256::Sha256::new();
        let mut piece = [0u8; HASH_PIECE];
        let mut done = 0;
        while done < len {
            let n = (len - done).min(HASH_PIECE);
            fetch(offset + done, &mut piece[..n]).map_err(|_| PkgError::Unreadable)?;
            h.update(&piece[..n]);
            done += n;
        }
        Ok(h.finish())
    };
    if digest(program)? != parsed.program_sha256 {
        return Err(PkgError::HashMismatch("program"));
    }
    if digest(app_manifest)? != parsed.manifest_sha256 {
        return Err(PkgError::HashMismatch("program.manifest"));
    }
    Ok(StreamVerified {
        manifest: parsed,
        program,
        app_manifest,
    })
}

/// What a package says it is, read piece by piece without hashing its
/// program (ADR 0074 phase C: the launcher's list): the package manifest's
/// signature is checked, and its application manifest is read into
/// `app_out` and checked against the signed hash, so both are what the
/// release key signed. The program is not checked: whoever runs the
/// package verifies it whole first ([`verify`], [`verify_streaming`]).
/// Returns the package manifest and the application manifest's length; an
/// application manifest longer than `app_out` is `BadManifest`.
pub fn inspect_streaming(
    len: usize,
    mut read: impl FnMut(usize, &mut [u8]) -> Result<(), ()>,
    pubkey: &[u8; 32],
    app_out: &mut [u8],
) -> Result<(PkgManifest, usize), PkgError> {
    let unreadable = core::cell::Cell::new(false);
    let mut fetch = |offset: usize, buf: &mut [u8]| {
        read(offset, buf).map_err(|()| {
            unreadable.set(true);
            aricode_initrd::InitrdError::Truncated
        })
    };
    let (parsed, _, (offset, app_len)) = signed_head(len, &mut fetch, pubkey, &unreadable)?;
    let app = app_out.get_mut(..app_len).ok_or(PkgError::BadManifest)?;
    fetch(offset, app).map_err(|_| PkgError::Unreadable)?;
    if sha256::sha256(app) != parsed.manifest_sha256 {
        return Err(PkgError::HashMismatch("program.manifest"));
    }
    Ok((parsed, app_len))
}

/// The four files of a package read piece by piece, and its manifest with
/// the signature checked: the manifest and the spans of the program and the
/// application manifest. `unreadable` tells a failed read from a bad
/// container.
fn signed_head(
    len: usize,
    fetch: &mut impl FnMut(usize, &mut [u8]) -> Result<(), aricode_initrd::InitrdError>,
    pubkey: &[u8; 32],
    unreadable: &core::cell::Cell<bool>,
) -> Result<(PkgManifest, aricode_initrd::Span, aricode_initrd::Span), PkgError> {
    let names = ["manifest", "manifest.sig", "program", "program.manifest"];
    let [manifest, sig, program, app_manifest] = aricode_initrd::locate(len, &mut *fetch, names)
        .map_err(|_| {
            if unreadable.get() {
                PkgError::Unreadable
            } else {
                PkgError::BadContainer
            }
        })?;
    let manifest = manifest.ok_or(PkgError::MissingFile("manifest"))?;
    let sig = sig.ok_or(PkgError::MissingFile("manifest.sig"))?;
    let program = program.ok_or(PkgError::MissingFile("program"))?;
    let app_manifest = app_manifest.ok_or(PkgError::MissingFile("program.manifest"))?;
    if sig.1 != ed25519::SIGNATURE_LEN {
        return Err(PkgError::BadSignature);
    }
    if manifest.1 > SMALL_FILE_MAX {
        return Err(PkgError::BadManifest);
    }
    let mut signature = [0u8; 64];
    fetch(sig.0, &mut signature).map_err(|_| PkgError::Unreadable)?;
    let mut manifest_buf = [0u8; SMALL_FILE_MAX];
    let manifest_bytes = &mut manifest_buf[..manifest.1];
    fetch(manifest.0, manifest_bytes).map_err(|_| PkgError::Unreadable)?;
    if !ed25519::verify(pubkey, manifest_bytes, &signature) {
        return Err(PkgError::BadSignature);
    }
    let text = core::str::from_utf8(manifest_bytes).map_err(|_| PkgError::BadManifest)?;
    let parsed = PkgManifest::parse(text).ok_or(PkgError::BadManifest)?;
    Ok((parsed, program, app_manifest))
}

/// Builds and signs a package (host tools); `requires` is its lowest
/// application interface level, 0 for none. With `corrupt` the signature
/// is altered so that it does not verify.
#[cfg(feature = "write")]
pub fn build(
    name: &str,
    version: u32,
    requires: u32,
    program: &[u8],
    app_manifest: &[u8],
    seed: &[u8; 32],
    corrupt: bool,
) -> Option<std::vec::Vec<u8>> {
    let manifest = PkgManifest {
        requires,
        ..PkgManifest::new(name, version, program, app_manifest)?
    };
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
        let elf = b"\x7fELF fake program";
        let app = b"name = demo\ncaps = system\n";
        let pkg = build("demo", 3, 0, elf, app, &SEED, false).unwrap();
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
    fn requires_is_signed_and_has_one_spelling() {
        let pk = ed25519::public_key(&SEED);
        let elf = b"\x7fELF";
        let app = b"name = demo\n";
        let pkg = build("demo", 2, 3, elf, app, &SEED, false).unwrap();
        let v = verify(&pkg, &pk).unwrap();
        assert_eq!(v.manifest.requires, 3);
        assert!(v.manifest.runs_on(3) && v.manifest.runs_on(4) && !v.manifest.runs_on(2));
        let mut text = [0u8; 256];
        let canonical = v.manifest.write(&mut text);
        assert!(canonical.ends_with("\nrequires = 3\n"), "{canonical}");
        assert_eq!(PkgManifest::parse(canonical), Some(v.manifest));
        // Without it, the manifest is the one packages always had.
        let plain = build("demo", 2, 0, elf, app, &SEED, false).unwrap();
        let old = verify(&plain, &pk).unwrap();
        assert!(!old.manifest.write(&mut text).contains("requires"));
        assert!(old.manifest.runs_on(0));
        // `requires = 0`, a repeated or a malformed line are refused.
        let base = std::string::String::from(old.manifest.write(&mut text));
        for extra in [
            "requires = 0\n",
            "requires = 2\nrequires = 2\n",
            "requires = x\n",
            "requires = 02\n",
            "requires = +2\n",
            "requires = \n",
        ] {
            assert_eq!(PkgManifest::parse(&(base.clone() + extra)), None, "{extra}");
        }
    }

    #[test]
    fn rejects_tampering() {
        let pk = ed25519::public_key(&SEED);
        let elf = b"\x7fELF";
        let app = b"name = demo\n";
        assert_eq!(
            verify(&build("demo", 1, 0, elf, app, &SEED, true).unwrap(), &pk).err(),
            Some(PkgError::BadSignature)
        );
        let other = ed25519::public_key(&[9u8; 32]);
        let pkg = build("demo", 1, 0, elf, app, &SEED, false).unwrap();
        assert_eq!(verify(&pkg, &other).err(), Some(PkgError::BadSignature));
        // Program changed after signing: the hash does not match.
        let mut tampered = pkg.clone();
        let off = verify(&pkg, &pk).unwrap().program_offset(&pkg);
        tampered[off + 1] ^= 0xFF;
        assert_eq!(
            verify(&tampered, &pk).err(),
            Some(PkgError::HashMismatch("program"))
        );
        assert_eq!(verify(b"garbage", &pk).err(), Some(PkgError::BadContainer));
        assert!(build("with space", 1, 0, elf, app, &SEED, false).is_none());
    }

    fn streaming(pkg: &[u8], pk: &[u8; 32]) -> Result<StreamVerified, PkgError> {
        verify_streaming(
            pkg.len(),
            |o, b| {
                b.copy_from_slice(pkg.get(o..o + b.len()).ok_or(())?);
                Ok(())
            },
            pk,
        )
    }

    #[test]
    fn inspection_checks_the_manifests_not_the_program() {
        let pk = ed25519::public_key(&SEED);
        let elf: std::vec::Vec<u8> = (0..50_000u32).map(|i| (i * 3) as u8).collect();
        let app = b"name = demo\ncaps = window\n";
        let pkg = build("demo", 2, 0, &elf, app, &SEED, false).unwrap();
        let inspect = |pkg: &[u8], out: &mut [u8]| {
            inspect_streaming(
                pkg.len(),
                |o, b| {
                    b.copy_from_slice(pkg.get(o..o + b.len()).ok_or(())?);
                    Ok(())
                },
                &pk,
                out,
            )
        };
        let mut out = [0u8; 64];
        let (m, n) = inspect(&pkg, &mut out).unwrap();
        assert_eq!((m.name_str(), m.version), ("demo", 2));
        assert_eq!(&out[..n], app);
        assert_eq!(
            inspect(&pkg, &mut [0u8; 8]).err(),
            Some(PkgError::BadManifest)
        );
        let v = streaming(&pkg, &pk).unwrap();
        // A changed program is not seen here (the launch sees it)...
        let mut program = pkg.clone();
        program[v.program.0 + 10] ^= 1;
        assert!(inspect(&program, &mut out).is_ok());
        assert!(streaming(&program, &pk).is_err());
        // ...a changed application manifest or signature is.
        let mut manifest = pkg.clone();
        manifest[v.app_manifest.0] ^= 1;
        assert_eq!(
            inspect(&manifest, &mut out).err(),
            Some(PkgError::HashMismatch("program.manifest"))
        );
        let bad = build("demo", 2, 0, &elf, app, &SEED, true).unwrap();
        assert_eq!(inspect(&bad, &mut out).err(), Some(PkgError::BadSignature));
    }

    #[test]
    fn streaming_verification_agrees_with_verify() {
        let pk = ed25519::public_key(&SEED);
        let elf: std::vec::Vec<u8> = (0..300_000u32).map(|i| (i * 7) as u8).collect();
        let app = b"name = demo\ncaps = system\n";
        let pkg = build("demo", 4, 0, &elf, app, &SEED, false).unwrap();
        let v = streaming(&pkg, &pk).unwrap();
        let whole = verify(&pkg, &pk).unwrap();
        assert_eq!(v.manifest, whole.manifest);
        assert_eq!(v.program, (whole.program_offset(&pkg), elf.len()));
        assert_eq!(&pkg[v.app_manifest.0..][..v.app_manifest.1], app);
        // The same refusals.
        let mut tampered = pkg.clone();
        tampered[v.program.0 + 1000] ^= 1;
        assert_eq!(
            streaming(&tampered, &pk).err(),
            Some(PkgError::HashMismatch("program"))
        );
        let other = ed25519::public_key(&[9u8; 32]);
        assert_eq!(streaming(&pkg, &other).err(), Some(PkgError::BadSignature));
        let bad = build("demo", 4, 0, &elf, app, &SEED, true).unwrap();
        assert_eq!(streaming(&bad, &pk).err(), Some(PkgError::BadSignature));
        assert_eq!(
            streaming(b"garbage", &pk).err(),
            Some(PkgError::BadContainer)
        );
        let failing = verify_streaming(pkg.len(), |_, _| Err(()), &pk);
        assert_eq!(failing.err(), Some(PkgError::Unreadable));
        // A read that fails after the headers (inside the program) too.
        let late = verify_streaming(
            pkg.len(),
            |o, b| {
                if o >= v.program.0 + 4096 {
                    return Err(());
                }
                b.copy_from_slice(&pkg[o..o + b.len()]);
                Ok(())
            },
            &pk,
        );
        assert_eq!(late.err(), Some(PkgError::Unreadable));
        // The package manifest's bound: refused over it, before the signature.
        let long = std::format!(
            "name = demo\nversion = 4\n#{}\n",
            "x".repeat(SMALL_FILE_MAX)
        );
        let c = aricode_initrd::build(&[
            ("manifest", long.as_bytes()),
            ("manifest.sig", &[0u8; 64][..]),
            ("program", &elf[..]),
            ("program.manifest", &app[..]),
        ])
        .unwrap();
        assert_eq!(streaming(&c, &pk).err(), Some(PkgError::BadManifest));
    }
}
