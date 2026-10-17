// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `updater` (Phase 30, ADR 0027): installs a system image into the inactive
//! slot and marks it pending with three tries; AriBoot tries it on the next
//! boot and init confirms it if the system comes up.
//!
//! Argument: `0` installs whatever the `UPDATE` partition holds; `1`
//! downloads the image from the update channel (URL in `update_url` of the
//! private directory) into `UPDATE` first; `2` only reports the installed
//! and available versions. Handles, in manifest order: SYSTEM, files (the
//! private directory), network, block service, hardware.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use aricode_ab::{schedule, BootState, Image, Slot, PART_STATE, PART_UPDATE};
use aricode_crypto::sha256::Sha256;
use aricode_rt::fmt_u64;
use aricode_rt::sys::block::BlockClient;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::net::NetClient;
use aricode_rt::sys::{self, Handle};

const SYSTEM: Handle = Handle(0x0001_0004);
const FS: Handle = Handle(0x0001_0005);
const NET: Handle = Handle(0x0001_0006);
const BLOCK: Handle = Handle(0x0001_0007);
const HW: Handle = Handle(0x0001_0008);

/// `0`: install from UPDATE (any other value than the two below).
const ARG_FETCH: u64 = 1;
const ARG_CHECK: u64 = 2;

const PUBKEY_HEX: &str = include_str!("../../../keys/dev/ed25519.pub");
/// DMA buffer where a whole image is loaded and verified.
const IMAGE_BUFFER: u64 = 4 * 1024 * 1024;
const CHUNK: u16 = 16;
const URL_FILE: &str = "update_url";
const MANIFEST_FILE: &str = "manifest.txt";
const HTTP_TIMEOUT_MS: u32 = 15_000;

/// Everything printed, kept for `last.log` (the shell shows it to a remote
/// session, whose user cannot see the console).
struct Log(core::cell::RefCell<Vec<u8>>);
// SAFETY: the updater is single-threaded.
unsafe impl Sync for Log {}
static LOG: Log = Log(core::cell::RefCell::new(Vec::new()));
const LOG_FILE: &str = "last.log";
const LOG_MAX: usize = 4096;

fn out(msg: &[u8]) {
    sys::print(msg);
    let mut log = LOG.0.borrow_mut();
    let room = LOG_MAX.saturating_sub(log.len());
    log.extend_from_slice(&msg[..msg.len().min(room)]);
}

fn say(msg: &[u8]) {
    out(b"updater: ");
    out(msg);
    out(b"\n");
}

fn num(v: u64) {
    let mut b = [0u8; 20];
    out(fmt_u64(v, &mut b));
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

/// Copies `sectors` sectors from `src` to `dst` through the client buffer.
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

/// The slot image of `part` loaded into `image` and verified: container
/// length and version. `None` if there is no image or it does not verify.
fn load_image(
    blk: &mut BlockClient,
    part: (u64, u64),
    image: &mut [u8],
    pk: &[u8; 32],
) -> Option<(u32, u32)> {
    blk.read(part.0, 1).ok()?;
    let len = Image::container_len(&blk.buffer()[..512]).ok()?;
    let sectors = (len as u64).div_ceil(512);
    if sectors * 512 > image.len() as u64 || sectors + 1 > part.1 - part.0 + 1 {
        return None;
    }
    let mut done = 0u64;
    while done < sectors {
        let n = (sectors - done).min(CHUNK as u64) as u16;
        blk.read(part.0 + 1 + done, n).ok()?;
        let o = done as usize * 512;
        image[o..o + n as usize * 512].copy_from_slice(&blk.buffer()[..n as usize * 512]);
        done += n as u64;
    }
    let version = Image::verify(&image[..len as usize], pk)
        .ok()?
        .manifest
        .version;
    Some((len, version))
}

/// The update channel manifest: `version`, `image`, `size`, `sha256`.
struct Channel {
    version: u32,
    image: String,
    size: usize,
    sha256: [u8; 32],
}

impl Channel {
    fn parse(text: &str) -> Option<Self> {
        let (mut version, mut image, mut size, mut sha) = (None, None, None, None);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line.split_once('=')?;
            let v = v.trim();
            match k.trim() {
                "version" => version = Some(v.parse().ok()?),
                "image" => image = Some(String::from(v)),
                "size" => size = Some(v.parse().ok()?),
                "sha256" => {
                    let mut h = [0u8; 32];
                    aricode_crypto::unhex(v, &mut h)?;
                    sha = Some(h);
                }
                _ => return None,
            }
        }
        let image: String = image?;
        if image.is_empty() || image.contains('/') {
            return None;
        }
        Some(Self {
            version: version?,
            image,
            size: size?,
            sha256: sha?,
        })
    }
}

fn read_text(fs: &mut FsClient, name: &str) -> Option<String> {
    let fd = fs.open(name).ok()?;
    let mut data = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        match fs.read(fd, data.len() as u32, &mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => data.extend_from_slice(&chunk[..n]),
        }
    }
    let _ = fs.close(fd);
    String::from_utf8(data).ok()
}

fn http_error(e: aricode_https::HttpsError) -> &'static [u8] {
    use aricode_https::HttpsError as E;
    match e {
        E::BadUrl => b"invalid channel URL",
        E::Resolve(_) => b"cannot resolve the channel host",
        E::Connect(_) => b"cannot connect to the channel",
        E::Io(_) => b"connection lost",
        E::Tls(_) => b"TLS failure (certificate or protocol)",
        E::Malformed => b"invalid HTTP response",
        E::TooBig => b"response headers too large",
        E::Unsupported => b"unsupported HTTP response",
        E::Aborted => b"write error while saving the image",
    }
}

/// Fetches the channel manifest at `url`.
fn channel(net: &mut NetClient, url: &str) -> Option<Channel> {
    let mut text = [0u8; 1024];
    let full = alloc::format!("{url}{MANIFEST_FILE}");
    match aricode_https::get(net, SYSTEM, &[], &full, &mut text, HTTP_TIMEOUT_MS) {
        Ok(f) if f.status == 200 => {
            let parsed = core::str::from_utf8(&text[..f.body_len])
                .ok()
                .and_then(Channel::parse);
            if parsed.is_none() {
                say(b"invalid channel manifest");
            }
            parsed
        }
        Ok(f) => {
            out(b"updater: channel manifest: HTTP ");
            num(f.status as u64);
            out(b"\n");
            None
        }
        Err(e) => {
            out(b"updater: channel manifest: ");
            out(http_error(e));
            out(b"\n");
            None
        }
    }
}

/// Downloads the channel image straight into the UPDATE partition, sector
/// by sector, hashing it on the way; the header sector is written last so a
/// cut download never leaves a plausible image behind.
fn download(
    net: &mut NetClient,
    blk: &mut BlockClient,
    url: &str,
    ch: &Channel,
    update: (u64, u64),
) -> bool {
    let capacity = (update.1 - update.0 + 1) * 512;
    if ch.size as u64 > capacity || ch.size <= 512 {
        say(b"channel image does not fit the UPDATE partition");
        return false;
    }
    out(b"updater: downloading ");
    out(ch.image.as_bytes());
    out(b" (");
    num(ch.size as u64);
    out(b" bytes)\n");
    // Invalidate any previous image first.
    blk.buffer()[..512].fill(0);
    if blk.write(update.0, 1).is_err() {
        say(b"cannot write the UPDATE partition");
        return false;
    }
    let mut hash = Sha256::new();
    let mut header = [0u8; 512];
    let mut pending: Vec<u8> = Vec::with_capacity(CHUNK as usize * 512);
    let mut written: u64 = 0; // bytes placed in the partition image (header included)
    let mut failed = false;
    let flush = |blk: &mut BlockClient, pending: &mut Vec<u8>, written: &mut u64| -> bool {
        // `pending` holds whole sectors of the image at offset `written`.
        let sectors = (pending.len() / 512) as u16;
        if sectors == 0 {
            return true;
        }
        let bytes = sectors as usize * 512;
        blk.buffer()[..bytes].copy_from_slice(&pending[..bytes]);
        let lba = update.0 + *written / 512;
        if blk.write(lba, sectors).is_err() {
            return false;
        }
        pending.drain(..bytes);
        *written += bytes as u64;
        true
    };
    let mut sink = |chunk: &[u8]| {
        hash.update(chunk);
        let mut chunk = chunk;
        // The first 512 bytes are the header: kept aside until the end.
        while !chunk.is_empty() {
            let total = written + pending.len() as u64;
            if total < 512 {
                let n = (512 - total as usize).min(chunk.len());
                header[total as usize..total as usize + n].copy_from_slice(&chunk[..n]);
                chunk = &chunk[n..];
                if total as usize + n == 512 {
                    written = 512; // header slot reserved, written at the end
                }
                continue;
            }
            let room = CHUNK as usize * 512 - pending.len();
            let n = room.min(chunk.len());
            pending.extend_from_slice(&chunk[..n]);
            chunk = &chunk[n..];
            if pending.len() == CHUNK as usize * 512 && !flush(blk, &mut pending, &mut written) {
                failed = true;
                return false;
            }
        }
        true
    };
    let full = alloc::format!("{url}{}", ch.image);
    let status = match aricode_https::get_with(net, SYSTEM, &[], &full, HTTP_TIMEOUT_MS, &mut sink)
    {
        Ok(s) => s,
        Err(e) => {
            if !failed {
                out(b"updater: download failed: ");
                out(http_error(e));
                out(b"\n");
            } else {
                say(b"cannot write the UPDATE partition");
            }
            return false;
        }
    };
    if status != 200 {
        out(b"updater: image download: HTTP ");
        num(status as u64);
        out(b"\n");
        return false;
    }
    // Tail: pad the last partial sector and flush.
    if written < 512 {
        say(b"image too short");
        return false;
    }
    let total = written + pending.len() as u64;
    if total != ch.size as u64 {
        say(b"download size does not match the channel manifest");
        return false;
    }
    let pad = (512 - pending.len() % 512) % 512;
    pending.resize(pending.len() + pad, 0);
    if !flush(blk, &mut pending, &mut written) {
        say(b"cannot write the UPDATE partition");
        return false;
    }
    if hash.finish() != ch.sha256 {
        say(b"download SHA-256 does not match the channel manifest");
        return false;
    }
    blk.buffer()[..512].copy_from_slice(&header);
    if blk.write(update.0, 1).is_err() || blk.flush().is_err() {
        say(b"cannot write the UPDATE partition");
        return false;
    }
    say(b"download complete and SHA-256 verified");
    true
}

fn run(arg: u64) -> u64 {
    let Ok(mut blk) = BlockClient::new(BLOCK, HW, CHUNK) else {
        say(b"need the block service (block capability)");
        return 2;
    };
    let mut pk = [0u8; 32];
    if aricode_crypto::unhex(PUBKEY_HEX.trim(), &mut pk).is_none() {
        return 3;
    }
    // 1. Partitions and state.
    let Some(update) = find_partition(&mut blk, PART_UPDATE) else {
        say(b"no UPDATE partition");
        return 4;
    };
    let Some(state_part) = find_partition(&mut blk, PART_STATE) else {
        say(b"no ARIBOOT partition");
        return 5;
    };
    if blk.read(state_part.0, 1).is_err() {
        return 6;
    }
    let state = BootState::decode(&blk.buffer()[..512]).unwrap_or(BootState::fresh(Slot::A));
    let target = state.active.other();
    let (Some(active_part), Some(slot_part)) = (
        find_partition(&mut blk, state.active.partition()),
        find_partition(&mut blk, target.partition()),
    ) else {
        say(b"no slot partitions");
        return 7;
    };
    let Ok(dma) = sys::dma_alloc(HW, IMAGE_BUFFER) else {
        say(b"no memory for the image (quota)");
        return 11;
    };
    let Ok(virt) = sys::mmio_map(dma) else {
        return 11;
    };
    // SAFETY: our own DMA buffer of IMAGE_BUFFER bytes, mapped RW.
    let image = unsafe { core::slice::from_raw_parts_mut(virt as *mut u8, IMAGE_BUFFER as usize) };

    // 2. Network: channel manifest, version comparison, download.
    if arg == ARG_FETCH || arg == ARG_CHECK {
        let (Ok(mut fs), Ok(mut net)) = (FsClient::new(FS, HW), NetClient::new(NET, HW)) else {
            say(b"need the file and network services");
            return 20;
        };
        let Some(url) = read_text(&mut fs, URL_FILE) else {
            say(b"no update channel configured: update http://host:port/path/");
            return 21;
        };
        let mut url = String::from(url.trim());
        if !url.ends_with('/') {
            url.push('/');
        }
        let Some(ch) = channel(&mut net, &url) else {
            return 22;
        };
        let installed = load_image(&mut blk, active_part, image, &pk).map(|(_, v)| v);
        out(b"updater: installed version ");
        match installed {
            Some(v) => num(v as u64),
            None => out(b"unknown"),
        }
        out(b", channel offers version ");
        num(ch.version as u64);
        if installed == Some(ch.version) {
            out(b": up to date\n");
            return 0;
        }
        out(b"\n");
        if arg == ARG_CHECK {
            return 0;
        }
        if !download(&mut net, &mut blk, &url, &ch, update) {
            return 23;
        }
    }

    // 3. Load the UPDATE image and verify it.
    let Some((len, version)) = load_image(&mut blk, update, image, &pk) else {
        say(b"UPDATE does not contain a valid system image (missing, too large, or signature or hashes fail); not installing");
        return 13;
    };
    let sectors = (len as u64).div_ceil(512);
    if sectors + 1 > slot_part.1 - slot_part.0 + 1 {
        say(b"image does not fit the slot");
        return 10;
    }
    out(b"updater: UPDATE image version ");
    num(version as u64);
    out(b" verified (");
    num(len as u64);
    out(b" bytes)\n");
    // 4. Copy header + container to the inactive slot (sector by sector, disk
    //    to disk; the in-memory copy was only used for verification).
    if !copy_sectors(&mut blk, update.0, slot_part.0, sectors + 1) {
        say(b"error writing the slot");
        return 14;
    }
    out(b"updater: installed into slot ");
    out(target.name().as_bytes());
    out(b"\n");
    // 5. Schedule the slot: pending with tries.
    let next = schedule(state, target);
    blk.buffer()[..512].copy_from_slice(&next.encode());
    if blk.write(state_part.0, 1).is_err() {
        say(b"error writing the boot state");
        return 15;
    }
    if blk.read(state_part.0, 1).is_err() {
        return 16;
    }
    match BootState::decode(&blk.buffer()[..512]) {
        Some(s) if s.pending == Some((target, aricode_ab::DEFAULT_TRIES)) => {
            out(b"updater: state: active ");
            out(s.active.name().as_bytes());
            out(b", pending ");
            out(target.name().as_bytes());
            out(b" (");
            num(aricode_ab::DEFAULT_TRIES as u64);
            out(b" tries); reboot to try it\n");
            0
        }
        _ => {
            say(b"boot state read back does not match");
            17
        }
    }
}

fn main(arg: u64) -> u64 {
    let code = run(arg);
    // The log for the shell (private directory); best effort.
    if let Ok(mut fs) = FsClient::new(FS, HW) {
        let log = LOG.0.borrow();
        let _ = fs
            .open(LOG_FILE)
            .or_else(|_| fs.create(LOG_FILE))
            .and_then(|fd| {
                let r = fs.replace(fd, &log);
                let _ = fs.close(fd);
                r
            });
    }
    code
}

aricode_rt::entry!(main);
