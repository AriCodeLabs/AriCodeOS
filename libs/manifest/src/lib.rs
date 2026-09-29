//! Manifiesto de aplicación (Fase 28): un fichero `NOMBRE.manifest` en el
//! initrd, junto al programa, con lo que la aplicación pide. El broker (init)
//! lo lee, aplica su política y entrega solo esas capabilities.
//!
//! ```text
//! # comentario
//! name = hello_app
//! caps = system, fs
//! memory = 128K
//! read_files = hola.txt      # ficheros concretos de AriFS en modo lectura (ADR 0016)
//! ```

#![no_std]

pub const MAX_NAME: usize = 32;

/// Capacidades que una aplicación puede pedir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Caps(u32);

impl Caps {
    pub const SYSTEM: Caps = Caps(1 << 0);
    pub const FS: Caps = Caps(1 << 1);
    pub const NET: Caps = Caps(1 << 2);
    pub const WINDOW: Caps = Caps(1 << 3);
    pub const INPUT: Caps = Caps(1 << 4);
    /// Reservadas al sistema: nunca se conceden a una aplicación.
    pub const SPAWNER: Caps = Caps(1 << 5);
    pub const PCI: Caps = Caps(1 << 6);
    pub const FRAMEBUFFER: Caps = Caps(1 << 7);
    /// Acceso al servicio de bloques: solo para aplicaciones de sistema de la
    /// lista de confianza del broker (`updater`).
    pub const BLOCK: Caps = Caps(1 << 8);
    /// Espacio principal de fsd para instalar paquetes (ADR 0018): solo para
    /// `installer`.
    pub const INSTALL: Caps = Caps(1 << 9);

    pub const NONE: Caps = Caps(0);
    /// Las que el broker concede a aplicaciones normales.
    pub const APP_ALLOWED: Caps =
        Caps(Caps::SYSTEM.0 | Caps::FS.0 | Caps::NET.0 | Caps::WINDOW.0 | Caps::INPUT.0);

    const NAMES: [(&'static str, Caps); 10] = [
        ("system", Caps::SYSTEM),
        ("fs", Caps::FS),
        ("net", Caps::NET),
        ("window", Caps::WINDOW),
        ("input", Caps::INPUT),
        ("spawner", Caps::SPAWNER),
        ("pci", Caps::PCI),
        ("framebuffer", Caps::FRAMEBUFFER),
        ("block", Caps::BLOCK),
        ("install", Caps::INSTALL),
    ];

    /// Aplicaciones de sistema que además pueden pedir `block`.
    pub const TRUSTED_BLOCK: [&'static str; 1] = ["updater"];
    /// Aplicaciones de sistema que además pueden pedir `install`.
    pub const TRUSTED_INSTALL: [&'static str; 1] = ["installer"];

    /// Capacidades concedibles a la aplicación `name`.
    pub fn allowed_for(name: &str) -> Caps {
        let mut caps = Caps::APP_ALLOWED;
        if Self::TRUSTED_BLOCK.contains(&name) {
            caps = caps.union(Caps::BLOCK);
        }
        if Self::TRUSTED_INSTALL.contains(&name) {
            caps = caps.union(Caps::INSTALL);
        }
        caps
    }

    pub fn from_name(name: &str) -> Option<Caps> {
        Self::NAMES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, c)| *c)
    }
    pub const fn contains(self, other: Caps) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn union(self, other: Caps) -> Caps {
        Caps(self.0 | other.0)
    }
    /// Las de `self` que no están en `allowed`.
    pub const fn difference(self, allowed: Caps) -> Caps {
        Caps(self.0 & !allowed.0)
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub const fn bits(self) -> u32 {
        self.0
    }
    /// Nombres de las capacidades presentes, en orden fijo.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        Self::NAMES
            .iter()
            .filter(move |(_, c)| self.contains(*c))
            .map(|(n, _)| *n)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestError {
    /// Línea sin `clave = valor`.
    Syntax,
    UnknownKey,
    UnknownCap,
    BadName,
    BadMemory,
    /// Falta `name`.
    MissingName,
    /// Nombre de fichero inválido o más de `MAX_READ_FILES`.
    BadFile,
}

/// Límite de memoria por defecto cuando el manifiesto no lo indica.
pub const DEFAULT_MEMORY_KIB: u32 = 1024;
/// Límite máximo que un manifiesto puede pedir (8 MiB, `DMA_MAX_LEN`).
pub const MAX_MEMORY_KIB: u32 = 8 * 1024;

/// Ficheros compartidos en modo lectura que puede pedir un manifiesto.
pub const MAX_READ_FILES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Manifest {
    pub name: [u8; MAX_NAME],
    pub name_len: u8,
    pub caps: Caps,
    /// Memoria (buffers DMA) que puede reservar, en KiB.
    pub memory_kib: u32,
    /// Ficheros de AriFS (nombre completo) que recibe en modo lectura, en
    /// el orden del manifiesto (ADR 0016).
    read_files: [[u8; MAX_NAME]; MAX_READ_FILES],
    read_files_len: [u8; MAX_READ_FILES],
    read_files_count: u8,
}

impl Manifest {
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
    }

    /// Ficheros de solo lectura pedidos, en orden.
    pub fn read_files(&self) -> impl Iterator<Item = &str> + Clone {
        self.read_files
            .iter()
            .zip(&self.read_files_len)
            .take(self.read_files_count as usize)
            .map(|(n, len)| core::str::from_utf8(&n[..*len as usize]).unwrap_or("?"))
    }

    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let mut name: Option<&str> = None;
        let mut caps = Caps::NONE;
        let mut memory_kib = DEFAULT_MEMORY_KIB;
        let mut read_files = [[0u8; MAX_NAME]; MAX_READ_FILES];
        let mut read_files_len = [0u8; MAX_READ_FILES];
        let mut read_files_count = 0usize;
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or(ManifestError::Syntax)?;
            let value = value.trim();
            match key.trim() {
                "name" => {
                    if value.is_empty()
                        || value.len() > MAX_NAME
                        || !value
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                    {
                        return Err(ManifestError::BadName);
                    }
                    name = Some(value);
                }
                "caps" => {
                    for c in value.split(',').map(str::trim).filter(|c| !c.is_empty()) {
                        caps = caps.union(Caps::from_name(c).ok_or(ManifestError::UnknownCap)?);
                    }
                }
                "memory" => memory_kib = parse_memory(value).ok_or(ManifestError::BadMemory)?,
                "read_files" => {
                    for f in value.split(',').map(str::trim).filter(|f| !f.is_empty()) {
                        if !valid_file_name(f) || read_files_count == MAX_READ_FILES {
                            return Err(ManifestError::BadFile);
                        }
                        read_files[read_files_count][..f.len()].copy_from_slice(f.as_bytes());
                        read_files_len[read_files_count] = f.len() as u8;
                        read_files_count += 1;
                    }
                }
                _ => return Err(ManifestError::UnknownKey),
            }
        }
        let name = name.ok_or(ManifestError::MissingName)?;
        let mut n = [0u8; MAX_NAME];
        n[..name.len()].copy_from_slice(name.as_bytes());
        Ok(Self {
            name: n,
            name_len: name.len() as u8,
            caps,
            memory_kib,
            read_files,
            read_files_len,
            read_files_count: read_files_count as u8,
        })
    }
}

/// Nombre completo de AriFS: `[A-Za-z0-9._-/]`, sin empezar ni acabar en `/`.
fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("//")
}

/// `64K`, `2M` o KiB sin sufijo; acotado a [`MAX_MEMORY_KIB`].
fn parse_memory(v: &str) -> Option<u32> {
    let (num, mult) = match v.strip_suffix(['K', 'k']) {
        Some(n) => (n, 1u32),
        None => match v.strip_suffix(['M', 'm']) {
            Some(n) => (n, 1024u32),
            None => (v, 1u32),
        },
    };
    let n: u32 = num.trim().parse().ok()?;
    let kib = n.checked_mul(mult)?;
    (kib > 0 && kib <= MAX_MEMORY_KIB).then_some(kib)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    #[test]
    fn parses_full_manifest() {
        let m = Manifest::parse(
            "# app de ejemplo\nname = hello_app\ncaps = system, fs\nmemory = 128K\n",
        )
        .unwrap();
        assert_eq!(m.name_str(), "hello_app");
        assert!(m.caps.contains(Caps::SYSTEM) && m.caps.contains(Caps::FS));
        assert!(!m.caps.contains(Caps::NET));
        assert_eq!(m.memory_kib, 128);
        let names: Vec<&str> = m.caps.names().collect();
        assert_eq!(names, ["system", "fs"]);
        assert!(m.caps.difference(Caps::APP_ALLOWED).is_empty());
    }

    #[test]
    fn read_files_in_order_and_bounded() {
        let m = Manifest::parse("name = x\nread_files = hola.txt, docs/guia.md\n").unwrap();
        let files: Vec<&str> = m.read_files().collect();
        assert_eq!(files, ["hola.txt", "docs/guia.md"]);
        assert_eq!(
            Manifest::parse("name = x\nread_files = /abs"),
            Err(ManifestError::BadFile)
        );
        assert_eq!(
            Manifest::parse("name = x\nread_files = a, b, c, d, e"),
            Err(ManifestError::BadFile)
        );
        assert_eq!(Manifest::parse("name = x").unwrap().read_files().count(), 0);
    }

    #[test]
    fn defaults_and_units() {
        let m = Manifest::parse("name = x").unwrap();
        assert_eq!(m.memory_kib, DEFAULT_MEMORY_KIB);
        assert!(m.caps.is_empty());
        assert_eq!(
            Manifest::parse("name = x\nmemory = 2M").unwrap().memory_kib,
            2048
        );
        assert_eq!(
            Manifest::parse("name = x\nmemory = 512")
                .unwrap()
                .memory_kib,
            512
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(
            Manifest::parse("caps = fs"),
            Err(ManifestError::MissingName)
        );
        assert_eq!(
            Manifest::parse("name = x\ncolor = rojo"),
            Err(ManifestError::UnknownKey)
        );
        assert_eq!(
            Manifest::parse("name = x\ncaps = fs, laser"),
            Err(ManifestError::UnknownCap)
        );
        assert_eq!(
            Manifest::parse("name = con espacio"),
            Err(ManifestError::BadName)
        );
        assert_eq!(
            Manifest::parse("name = x\nmemory = 0"),
            Err(ManifestError::BadMemory)
        );
        assert_eq!(
            Manifest::parse("name = x\nmemory = 9M"),
            Err(ManifestError::BadMemory)
        );
        assert_eq!(Manifest::parse("sin igual"), Err(ManifestError::Syntax));
        let block = Manifest::parse("name = updater\ncaps = system, block").unwrap();
        assert!(block
            .caps
            .difference(Caps::allowed_for("updater"))
            .is_empty());
        assert!(
            !block.caps.difference(Caps::allowed_for("otra")).is_empty(),
            "block solo para updater"
        );
        let install = Manifest::parse("name = installer\ncaps = system, install").unwrap();
        assert!(install
            .caps
            .difference(Caps::allowed_for("installer"))
            .is_empty());
        assert!(
            !install
                .caps
                .difference(Caps::allowed_for("updater"))
                .is_empty(),
            "install solo para installer"
        );
        let forbidden = Manifest::parse("name = x\ncaps = fs, spawner").unwrap();
        let denied: Vec<&str> = forbidden
            .caps
            .difference(Caps::APP_ALLOWED)
            .names()
            .collect();
        assert_eq!(denied, ["spawner"]);
    }
}
