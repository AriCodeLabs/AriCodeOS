// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Key management (Phase 31): generate Ed25519 key pairs, sign and verify
//! files. The private key (32-byte seed, hex) never leaves the host; the
//! system only carries public keys.
//!
//! Two channels, two keys (ADR 0045): the release key signs the stable
//! channel (releases, packages, their indexes) and the development
//! channel's key signs development builds and their indexes. A run signs
//! one channel; every build embeds the release key and the development
//! channel's public key (the updater always, AriBoot only on a machine
//! that opted in).

use crate::Result;
use aricode_ab::{Channel, Keys};
use aricode_crypto::{ed25519, hex, unhex};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const SEED_FILE: &str = "ed25519.seed";
pub const PUB_FILE: &str = "ed25519.pub";
/// Development key pair checked into the repo: for tests and QEMU only.
pub const DEV_KEY_DIR: &str = "keys/dev";
/// The development channel's test pair, checked into the repo next to the
/// test key: for tests and QEMU only (its seed is public too).
pub const TEST_DEV_CHANNEL_DIR: &str = "keys/dev/dev-channel";
/// The public half of the development channel's real key (ADR 0045); its
/// seed lives outside the repository (`~/.config/aricodeos/dev-signing-key`).
pub const DEV_CHANNEL_PUB: &str = "keys/dev-channel/ed25519.pub";
/// The public half of the release key (ADR 0029, `keys/release`).
pub const RELEASE_PUB: &str = "keys/release/ed25519.pub";

/// Environment variable through which a build embeds a non-development
/// public key (`aricode_crypto::trust`).
pub const SIGNING_PUB_ENV: &str = "ARICODE_SIGNING_PUB";
/// Environment variable through which a build embeds the development
/// channel's public key (`aricode_crypto::trust::dev_channel_key`).
pub const DEV_CHANNEL_PUB_ENV: &str = "ARICODE_DEV_CHANNEL_PUB";

/// The keys of this run, verified at selection time and kept here: they
/// are never read again, so a later read error can never fall back to the
/// development key (re-audit, R1).
#[derive(Clone)]
struct Selected {
    /// The channel this run signs.
    channel: Channel,
    /// Directory of the seed that signs it.
    signer: PathBuf,
    /// The release key every build embeds.
    stable: [u8; 32],
    /// The development channel's key every updater embeds.
    dev: [u8; 32],
    /// Test keys (public seeds, `keys/dev`): the build embeds them without
    /// `ARICODE_SIGNING_PUB`.
    test: bool,
}

static SELECTED: std::sync::OnceLock<Selected> = std::sync::OnceLock::new();

/// AriBoot trusts the development channel (`usb-image --dev-source`).
static LOADER_DEV: AtomicBool = AtomicBool::new(false);

fn root_path(rel: &str) -> PathBuf {
    crate::root().join(rel)
}

/// The test keys of `channel`: `keys/dev` signs the stable channel,
/// `keys/dev/dev-channel` the development one.
fn test_selection(channel: Channel) -> Result<Selected> {
    let signer = root_path(match channel {
        Channel::Stable => DEV_KEY_DIR,
        Channel::Dev => TEST_DEV_CHANNEL_DIR,
    });
    Ok(Selected {
        channel,
        signer,
        stable: read_hex(&root_path(DEV_KEY_DIR).join(PUB_FILE))?,
        dev: read_hex(&root_path(TEST_DEV_CHANNEL_DIR).join(PUB_FILE))?,
        test: true,
    })
}

/// The selection of this run: the one made, or the test keys of the
/// stable channel.
fn current() -> Result<Selected> {
    match SELECTED.get() {
        Some(s) => Ok(s.clone()),
        None => test_selection(Channel::Stable),
    }
}

/// The public keys whose seeds are in the repository.
fn public_seed_keys() -> Result<[[u8; 32]; 2]> {
    Ok([
        read_hex(&root_path(DEV_KEY_DIR).join(PUB_FILE))?,
        read_hex(&root_path(TEST_DEV_CHANNEL_DIR).join(PUB_FILE))?,
    ])
}

/// The rules for a real key pair of `channel` (ADR 0045, decision 3):
/// `public` is the pair's public key, `dev_channel` the committed public
/// half of the development channel's key, `release` the committed release
/// key, `public_seeds` the keys whose seeds are in the repository. Returns
/// the (stable, dev) keys the build embeds.
fn real_keys(
    channel: Channel,
    public: [u8; 32],
    dev_channel: [u8; 32],
    release: [u8; 32],
    public_seeds: &[[u8; 32]],
) -> Result<([u8; 32], [u8; 32])> {
    if public_seeds.contains(&public) {
        return Err(String::from(
            "a test key whose seed is in the repository: sign without --signing-key for tests",
        ));
    }
    if public_seeds.contains(&dev_channel) || dev_channel == release {
        return Err(format!(
            "{DEV_CHANNEL_PUB} must be the development channel's own key (not a test key, not the release key)"
        ));
    }
    match channel {
        Channel::Stable if public == dev_channel => Err(format!(
            "this is the development channel's key ({DEV_CHANNEL_PUB}): it signs --channel dev only"
        )),
        Channel::Stable => Ok((public, dev_channel)),
        Channel::Dev if public != dev_channel => Err(format!(
            "not the development channel's key: its public half must be {DEV_CHANNEL_PUB}"
        )),
        Channel::Dev => Ok((release, dev_channel)),
    }
}

/// `--signing-key DIR` for the stable channel (ADR 0029, "The signing
/// key").
pub fn select_signing_key(dir: Option<&Path>) -> Result {
    select_keys(dir, Channel::Stable)
}

/// `--signing-key DIR` with `--channel` (ADR 0029, ADR 0045): this run
/// signs `channel` with `DIR/ed25519.seed`. The pair must match, and a
/// real seed must live outside the repository. Without a directory the
/// test keys sign, with a loud warning: their seeds are public.
pub fn select_keys(dir: Option<&Path>, channel: Channel) -> Result {
    let test_dir = root_path(match channel {
        Channel::Stable => DEV_KEY_DIR,
        Channel::Dev => TEST_DEV_CHANNEL_DIR,
    });
    let is_test = match dir {
        None => true,
        Some(d) => {
            d.canonicalize()
                .map_err(|e| format!("--signing-key {}: {e}", d.display()))?
                == test_dir.canonicalize().unwrap_or_else(|_| test_dir.clone())
        }
    };
    if is_test {
        eprintln!(
            "[xtask] WARNING: signing the {} channel with the TEST key {}, whose seed is public: \
             anyone on the network can sign what this build accepts. \
             Use --signing-key DIR (cargo xtask keygen DIR, kept outside the repository) \
             before upgrading machines over an untrusted network.",
            channel.name(),
            test_dir
                .strip_prefix(crate::root())
                .unwrap_or(&test_dir)
                .display()
        );
        if channel == Channel::Stable {
            return Ok(());
        }
        return set(test_selection(channel)?);
    }
    let dir = dir
        .expect("not a test selection")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if dir.starts_with(crate::root()) {
        return Err(format!(
            "--signing-key {}: a signing seed must be kept outside the repository",
            dir.display()
        ));
    }
    let seed: [u8; 32] = read_hex(&dir.join(SEED_FILE))?;
    let public: [u8; 32] = read_hex(&dir.join(PUB_FILE))?;
    if ed25519::public_key(&seed) != public {
        return Err(format!(
            "--signing-key {}: {SEED_FILE} and {PUB_FILE} are not a pair",
            dir.display()
        ));
    }
    let (stable, dev) = real_keys(
        channel,
        public,
        read_hex(&root_path(DEV_CHANNEL_PUB))?,
        read_hex(&root_path(RELEASE_PUB))?,
        &public_seed_keys()?,
    )
    .map_err(|e| format!("--signing-key {}: {e}", dir.display()))?;
    println!(
        "[xtask] signing the {} channel with {} (public {})",
        channel.name(),
        dir.display(),
        hex_string(&public)
    );
    set(Selected {
        channel,
        signer: dir,
        stable,
        dev,
        test: false,
    })
}

fn set(selected: Selected) -> Result {
    SELECTED
        .set(selected)
        .map_err(|_| String::from("--signing-key given twice"))
}

fn hex_string(key: &[u8; 32]) -> String {
    let mut buf = [0u8; 64];
    hex(key, &mut buf).to_string()
}

/// Seed of the stable channel's signing key (the test one by default).
pub fn signing_seed() -> Result<[u8; 32]> {
    signing_seed_for(Channel::Stable)
}

/// Seed that signs `channel` in this run: a run with a real key signs only
/// that key's channel; the test keys sign both.
pub fn signing_seed_for(channel: Channel) -> Result<[u8; 32]> {
    let s = current()?;
    let dir = if s.test {
        test_selection(channel)?.signer
    } else if channel == s.channel {
        s.signer
    } else {
        return Err(format!(
            "this run holds the {} channel's key only; the {} channel is signed by another run",
            s.channel.name(),
            channel.name()
        ));
    };
    read_hex(&dir.join(SEED_FILE))
}

/// Public key (hex) a build must embed instead of the test one, as
/// verified at selection; `None` with the test keys.
pub fn signing_public_hex() -> Option<String> {
    SELECTED
        .get()
        .filter(|s| !s.test)
        .map(|s| hex_string(&s.stable))
}

/// The development channel's public key (hex) the build embeds.
pub fn dev_channel_public_hex() -> Result<String> {
    Ok(hex_string(&current()?.dev))
}

/// The release key the build must embed and every stable signature must
/// verify with: the selected one, or the test one (whose file must read).
pub fn expected_public_key() -> Result<[u8; 32]> {
    Ok(current()?.stable)
}

/// Both keys of the build, as a verifier that trusts both holds them.
pub fn expected_keys() -> Result<Keys> {
    let s = current()?;
    Ok(Keys {
        stable: s.stable,
        dev: Some(s.dev),
    })
}

/// `usb-image --dev-source`: AriBoot of this image trusts the development
/// channel (ADR 0045, decision 3).
pub fn set_loader_dev_channel() {
    LOADER_DEV.store(true, Ordering::Relaxed);
}

/// Whether AriBoot of this build trusts the development channel: on a
/// stick that opted in, and in test builds (whose keys are public anyway),
/// so one build serves every scenario.
pub fn loader_trusts_dev() -> bool {
    LOADER_DEV.load(Ordering::Relaxed) || current().is_ok_and(|s| s.test)
}

/// The keys found after `tag` in `data`.
fn anchors<'a>(data: &'a [u8], tag: &[u8; 16]) -> Vec<&'a [u8]> {
    data.windows(tag.len() + 32)
        .filter(|w| w[..tag.len()] == *tag)
        .map(|w| &w[tag.len()..])
        .collect()
}

/// Post-build check (re-audit, R1): `binary` must carry the trust anchor
/// (`aricode_crypto::trust`) with the expected public key, so a build that
/// embedded another key fails here instead of on the machines.
pub fn check_embedded_key(binary: &Path) -> Result {
    let data = fs::read(binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    let want = expected_public_key()?;
    match anchors(&data, &aricode_crypto::trust::ANCHOR_TAG).as_slice() {
        [key] if *key == want => Ok(()),
        [] => Err(format!("{}: no trust anchor found", binary.display())),
        _ => Err(format!(
            "{}: embeds a different public key than the one selected for signing",
            binary.display()
        )),
    }
}

/// Post-build check of the development channel's anchor (ADR 0045):
/// `binary` carries it with the expected key when `trusted`, and carries
/// no usable one otherwise (absent, or the all-zero placeholder of a
/// build without the key), so least privilege is checked, not assumed.
pub fn check_dev_anchor(binary: &Path, trusted: bool) -> Result {
    let data = fs::read(binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    let want = current()?.dev;
    let found = anchors(&data, &aricode_crypto::trust::DEV_ANCHOR_TAG);
    let usable: Vec<&&[u8]> = found.iter().filter(|k| k.iter().any(|&b| b != 0)).collect();
    match (trusted, usable.as_slice()) {
        (true, [key]) if **key == want => Ok(()),
        (true, []) => Err(format!(
            "{}: does not carry the development channel's key",
            binary.display()
        )),
        (true, _) => Err(format!(
            "{}: carries another development channel key than the selected one",
            binary.display()
        )),
        (false, []) => Ok(()),
        (false, _) => Err(format!(
            "{}: carries the development channel's key but must not trust that channel",
            binary.display()
        )),
    }
}

fn os_random(out: &mut [u8]) -> Result {
    // `/dev/urandom` is endless: read exactly what is needed, never `fs::read`.
    use std::io::Read;
    let mut f = fs::File::open("/dev/urandom").map_err(|e| format!("/dev/urandom: {e}"))?;
    f.read_exact(out)
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    Ok(())
}

pub fn read_hex<const N: usize>(path: &Path) -> Result<[u8; N]> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = [0u8; N];
    unhex(text.trim(), &mut out)
        .ok_or_else(|| format!("{}: expected {} bytes of hex", path.display(), N))?;
    Ok(out)
}

fn hex_line(bytes: &[u8]) -> String {
    let mut buf = vec![0u8; bytes.len() * 2];
    format!("{}\n", hex(bytes, &mut buf))
}

fn write_hex(path: &Path, bytes: &[u8]) -> Result {
    fs::write(path, hex_line(bytes)).map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes a secret: exclusive creation with mode 0600 from the very first
/// instant (without relying on the umask or a later chmod).
fn write_secret(path: &Path, bytes: &[u8]) -> Result {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(hex_line(bytes).as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// `xtask keygen DIR`: creates `DIR/ed25519.seed` and `DIR/ed25519.pub`.
pub fn keygen(dir: &Path) -> Result {
    // Directories created here are the owner's only (0700): they hold a seed.
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let seed_path = dir.join(SEED_FILE);
    if seed_path.exists() {
        return Err(format!(
            "{} already exists; keys are never overwritten",
            seed_path.display()
        ));
    }
    let mut seed = [0u8; ed25519::SEED_LEN];
    os_random(&mut seed)?;
    let pk = ed25519::public_key(&seed);
    write_secret(&seed_path, &seed)?;
    write_hex(&dir.join(PUB_FILE), &pk)?;
    let mut buf = [0u8; 64];
    println!("[xtask] public key {}", hex(&pk, &mut buf));
    Ok(())
}

/// `xtask sign SEED FILE`: writes `FILE.sig` (hex).
pub fn sign(seed_path: &Path, file: &Path) -> Result {
    let seed: [u8; 32] = read_hex(seed_path)?;
    let data = fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let sig = ed25519::sign(&seed, &data);
    let out = sig_path(file);
    write_hex(&out, &sig)?;
    println!("[xtask] signature at {}", out.display());
    Ok(())
}

/// `xtask verify PUB FILE [SIG]`.
pub fn verify(pub_path: &Path, file: &Path, sig: Option<&Path>) -> Result {
    let pk: [u8; 32] = read_hex(pub_path)?;
    let data = fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let sig_file = sig.map(Path::to_path_buf).unwrap_or_else(|| sig_path(file));
    let signature: [u8; 64] = read_hex(&sig_file)?;
    if ed25519::verify(&pk, &data, &signature) {
        println!("[xtask] valid signature: {}", file.display());
        Ok(())
    } else {
        Err(format!("INVALID signature: {}", file.display()))
    }
}

pub fn sig_path(file: &Path) -> std::path::PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".sig");
    file.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keygen_sign_verify_round_trip() {
        let dir = std::env::temp_dir().join(format!("aricode-keys-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        keygen(&dir).unwrap();
        assert!(keygen(&dir).is_err(), "does not overwrite");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join(SEED_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o077,
                0,
                "the seed must not be readable by others: {mode:o}"
            );
        }
        let file = dir.join("message.txt");
        fs::write(&file, b"test manifest").unwrap();
        sign(&dir.join(SEED_FILE), &file).unwrap();
        verify(&dir.join(PUB_FILE), &file, None).unwrap();
        fs::write(&file, b"tampered manifest").unwrap();
        assert!(verify(&dir.join(PUB_FILE), &file, None).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn signing_key_must_be_a_pair_outside_the_repository() {
        // Audit 1 (production key): a seed inside the repository, or a seed
        // and public key that do not match, are refused before anything
        // is signed.
        let err = select_signing_key(Some(&crate::root().join("keys"))).unwrap_err();
        assert!(err.contains("outside the repository"), "{err}");
        let tmp = crate::test_util::TempDir::new("signing-pair");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        keygen(&a).unwrap();
        keygen(&b).unwrap();
        fs::copy(b.join(PUB_FILE), a.join(PUB_FILE)).unwrap();
        let err = select_signing_key(Some(&a)).unwrap_err();
        assert!(err.contains("not a pair"), "{err}");
        assert!(signing_public_hex().is_none(), "nothing was selected");
    }

    #[test]
    fn each_channel_signs_with_its_own_real_key() {
        // ADR 0045, decision 3: the release key never signs the development
        // channel, the development key never the stable one, and keys whose
        // seed is public are never a real channel's key.
        let (release, dev, other) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let seeds = [[8u8; 32], [9u8; 32]];
        assert_eq!(
            real_keys(Channel::Stable, release, dev, release, &seeds),
            Ok((release, dev))
        );
        // Another stable key (the owner's own) works the same way.
        assert_eq!(
            real_keys(Channel::Stable, other, dev, release, &seeds),
            Ok((other, dev))
        );
        assert_eq!(
            real_keys(Channel::Dev, dev, dev, release, &seeds),
            Ok((release, dev)),
            "a development build embeds the release key for the way back"
        );
        let err = real_keys(Channel::Stable, dev, dev, release, &seeds).unwrap_err();
        assert!(err.contains("--channel dev only"), "{err}");
        let err = real_keys(Channel::Dev, release, dev, release, &seeds).unwrap_err();
        assert!(err.contains("not the development channel's key"), "{err}");
        let err = real_keys(Channel::Dev, other, dev, release, &seeds).unwrap_err();
        assert!(err.contains("not the development channel's key"), "{err}");
        for channel in [Channel::Stable, Channel::Dev] {
            let err = real_keys(channel, seeds[0], dev, release, &seeds).unwrap_err();
            assert!(err.contains("test key"), "{err}");
            let err = real_keys(channel, dev, seeds[1], release, &seeds).unwrap_err();
            assert!(err.contains("own key"), "{err}");
            let err = real_keys(channel, dev, release, release, &seeds).unwrap_err();
            assert!(err.contains("own key"), "{err}");
        }
    }

    #[test]
    fn committed_keys_follow_the_rules() {
        // The committed public halves: the development channel's key is
        // neither the release key nor a test key, and the test keys differ.
        let dev = read_hex::<32>(&root_path(DEV_CHANNEL_PUB)).unwrap();
        let release = read_hex::<32>(&root_path(RELEASE_PUB)).unwrap();
        let seeds = public_seed_keys().unwrap();
        assert_eq!(
            real_keys(Channel::Dev, dev, dev, release, &seeds),
            Ok((release, dev))
        );
        assert_ne!(seeds[0], seeds[1]);
        // The test pair of the development channel is a pair.
        let dir = root_path(TEST_DEV_CHANNEL_DIR);
        let seed = read_hex::<32>(&dir.join(SEED_FILE)).unwrap();
        assert_eq!(ed25519::public_key(&seed), seeds[1]);
        // Without a selection the test keys sign both channels.
        assert!(signing_seed_for(Channel::Dev).is_ok());
        assert_eq!(expected_keys().unwrap().dev, Some(seeds[1]));
    }

    #[test]
    fn dev_anchor_check_enforces_least_privilege() {
        let tmp = crate::test_util::TempDir::new("dev-anchor");
        let dev = expected_keys().unwrap().dev.unwrap();
        let file = |name: &str, key: Option<[u8; 32]>| {
            let mut bin = b"junk".to_vec();
            if let Some(k) = key {
                bin.extend_from_slice(&aricode_crypto::trust::DEV_ANCHOR_TAG);
                bin.extend_from_slice(&k);
            }
            bin.extend_from_slice(b"more");
            let path = tmp.path().join(name);
            fs::write(&path, &bin).unwrap();
            path
        };
        let with = file("with", Some(dev));
        let zero = file("zero", Some([0; 32]));
        let none = file("none", None);
        let other = file("other", Some([7; 32]));
        check_dev_anchor(&with, true).unwrap();
        check_dev_anchor(&none, false).unwrap();
        check_dev_anchor(&zero, false).unwrap();
        assert!(check_dev_anchor(&with, false)
            .unwrap_err()
            .contains("must not trust"));
        assert!(check_dev_anchor(&none, true)
            .unwrap_err()
            .contains("does not carry"));
        assert!(check_dev_anchor(&zero, true)
            .unwrap_err()
            .contains("does not carry"));
        assert!(check_dev_anchor(&other, true)
            .unwrap_err()
            .contains("another development channel key"));
    }

    #[test]
    fn embedded_key_check_finds_the_anchor() {
        let tmp = crate::test_util::TempDir::new("anchor");
        let dev = expected_public_key().unwrap();
        let mut bin = b"junk".to_vec();
        bin.extend_from_slice(&aricode_crypto::trust::ANCHOR_TAG);
        bin.extend_from_slice(&dev);
        bin.extend_from_slice(b"more");
        let good = tmp.path().join("good");
        fs::write(&good, &bin).unwrap();
        check_embedded_key(&good).unwrap();
        let n = bin.len();
        bin[n - 5] ^= 1; // last key byte
        let bad = tmp.path().join("bad");
        fs::write(&bad, &bin).unwrap();
        assert!(check_embedded_key(&bad)
            .unwrap_err()
            .contains("different public key"));
        let none = tmp.path().join("none");
        fs::write(&none, b"no anchor here").unwrap();
        assert!(check_embedded_key(&none)
            .unwrap_err()
            .contains("no trust anchor"));
    }
}
