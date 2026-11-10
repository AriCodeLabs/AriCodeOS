// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Render real game frames on the host, without a compositor or a GPU.
//! cargo run -p aricode-pista --release --example preview -- build/pista-preview
use aricode_pista::{
    effects,
    hud::{self, Overlay},
    raster::Target,
    render::{draw, View},
    world::{World, COUNTDOWN},
};
use std::{fs, io::Write, time::Instant};

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "build/pista-preview".into());
    fs::create_dir_all(&dir).unwrap();
    let (width, height) = (960, 540);
    let mut pixels = vec![0; width * height];
    let mut depth = vec![0; width * height];
    let (mut world, mut view) = (World::new(), View::default());
    let start = Instant::now();
    let mut frames = 0;
    for (name, step) in [
        ("grid", 1),
        ("race", COUNTDOWN + 500),
        ("summit", COUNTDOWN + 1100),
    ] {
        while world.step < step {
            let input = world.autopilot(0);
            world.advance(input);
            draw(
                &world,
                &mut view,
                &mut Target {
                    pixels: &mut pixels,
                    depth: &mut depth,
                    width,
                    height,
                },
            );
            effects::finish(&pixels, &mut depth, width, height);
            frames += 1;
        }
        pixels.copy_from_slice(&depth);
        hud::draw(
            &mut Target {
                pixels: &mut pixels,
                depth: &mut [],
                width,
                height,
            },
            &world,
            &Overlay {
                high_quality: true,
                ..Overlay::default()
            },
        );
        let mut output = fs::File::create(format!("{dir}/{name}.ppm")).unwrap();
        write!(output, "P6\n{width} {height}\n255\n").unwrap();
        let bytes: Vec<_> = pixels
            .iter()
            .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8])
            .collect();
        output.write_all(&bytes).unwrap();
    }
    println!(
        "{frames} native 960x540 frames, {:.2} ms/frame (host, CPU geometry + bloom, without compositor)",
        start.elapsed().as_secs_f64() * 1000.0 / frames as f64
    );
    println!("Frames: {dir}/grid.ppm, race.ppm, summit.ppm");
}
