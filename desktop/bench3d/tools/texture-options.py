#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
"""Prepare equivalent procedural/RGB24/QOI bench3d packages outside the checkout.

Usage: python3 -I desktop/bench3d/tools/texture-options.py ~/aricode-scratch/texture-options
Before each cargo xtask app-pack <output>/<procedural|raw|qoi> --version 1, run:
  cargo clean --manifest-path <candidate>/Cargo.toml \
    --target-dir build/external/bench3d-v1 --target x86_64-unknown-linux-gnu --release -p desktop-bench3d
Clean/rebuild when returning to desktop/bench3d too: the output binary path is shared.
The optional comparison dependency is pure Rust qoi =0.4.1 (MIT OR Apache-2.0),
with default features disabled. It is never a dependency of the shipped app.
"""
import pathlib
import re
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]
APP = ROOT / "desktop/bench3d"
DEST = pathlib.Path(sys.argv[1]).expanduser().resolve()
if DEST == pathlib.Path("/tmp") or pathlib.Path("/tmp") in DEST.parents:
    raise SystemExit("Use an output directory outside /tmp")
DEST.mkdir(parents=True, exist_ok=True)
PROBE = DEST / "probe"
(PROBE / "src").mkdir(parents=True, exist_ok=True)
(PROBE / "Cargo.toml").write_text(f'''[package]
name = "texture-options-probe"
version = "0.1.0"
edition = "2021"
[dependencies]
aricode-render3d = {{ path = "{ROOT}/libs/render3d" }}
qoi = {{ version = "=0.4.1", default-features = false, features = ["alloc"] }}
[profile.release]
opt-level = 3
lto = true
[workspace]
''')
(PROBE / "src/main.rs").write_text('''
#[path = "MATERIAL_PATH"]
mod materials;
fn main() {
    let output = std::path::Path::new("OUTPUT_PATH");
    for (i, size) in materials::SIZES.into_iter().enumerate() {
        let mut pixels = Vec::new();
        for y in 0..size {
            for x in 0..size {
                let c = materials::texel(i, x, y);
                pixels.extend_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8]);
            }
        }
        let qoi = qoi::encode_to_vec(&pixels, size as u32, size as u32).unwrap();
        std::fs::write(output.join(format!("{i}.rgb")), &pixels).unwrap();
        std::fs::write(output.join(format!("{i}.qoi")), &qoi).unwrap();
        assert_eq!(qoi::decode_to_vec(&qoi).unwrap().1, pixels);
        println!("material={i} raw_bytes={} qoi_bytes={}", pixels.len(), qoi.len());
    }
    let start = std::time::Instant::now();
    for _ in 0..100 {
        std::hint::black_box(materials::make().unwrap());
    }
    println!("host_proc_mean_us={}", start.elapsed().as_micros() / 100);
}
'''.replace("MATERIAL_PATH", str(APP / "src/materials.rs")).replace("OUTPUT_PATH", str(DEST)))
subprocess.run(["cargo", "run", "--release", "--manifest-path", str(PROBE / "Cargo.toml")], check=True)

for mode in ("procedural", "raw", "qoi"):
    target = DEST / mode
    (target / "src").mkdir(parents=True, exist_ok=True)
    for name in ("build.rs", "bench3d.manifest", "Cargo.lock"):
        shutil.copy2(APP / name, target / name)
    shutil.copy2(APP / "src/main.rs", target / "src/main.rs")
    manifest = (APP / "Cargo.toml").read_text()
    manifest = re.sub(r'path = "(\.\./[^\"]+)"',
                      lambda m: 'path = "' + str((APP / m[1]).resolve()) + '"', manifest)
    if mode == "qoi":
        manifest = manifest.replace("[dependencies]", '[dependencies]\nqoi = { version = "=0.4.1", default-features = false }')
    (target / "Cargo.toml").write_text(manifest)
    if mode == "procedural":
        shutil.copy2(APP / "src/materials.rs", target / "src/materials.rs")
        continue
    extension = "rgb" if mode == "raw" else "qoi"
    for i in range(4):
        shutil.copy2(DEST / f"{i}.{extension}", target / "src" / f"{i}.{extension}")
    source = '''use aricode_render3d::Texture;
const SIZES: [usize; 4] = [128, 128, 128, 64];
const DATA: [&[u8]; 4] = [include_bytes!("0.EXT"), include_bytes!("1.EXT"),
    include_bytes!("2.EXT"), include_bytes!("3.EXT")];
pub fn make() -> Option<[Texture; 4]> {
    Some([load(0)?, load(1)?, load(2)?, load(3)?])
}
fn load(i: usize) -> Option<Texture> {
    let size = SIZES[i];
    DECODE
    Texture::new(size, |x, y| {
        let j = (y * size + x) * 3;
        ((raw[j] as u32) << 16) | ((raw[j + 1] as u32) << 8) | raw[j + 2] as u32
    })
}
'''.replace("EXT", extension)
    decode = "let raw = DATA[i];" if mode == "raw" else '''let mut raw = alloc::vec::Vec::new();
    raw.try_reserve_exact(size * size * 3).ok()?;
    raw.resize(size * size * 3, 0);
    let header = qoi::decode_to_buf(&mut raw, DATA[i]).ok()?;
    if header.width != size as u32 || header.height != size as u32 {
        return None;
    }'''
    (target / "src/materials.rs").write_text(source.replace("DECODE", decode))
    print(target)
