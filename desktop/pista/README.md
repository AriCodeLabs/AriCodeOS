# Pista — Alpine Grand Prix

A three-lap arcade race against three rivals, built into AriCodeOS. The alpine
circuit has climbs and drops, linked bends and three boost zones. Spanish:
[README.es.md](README.es.md).

## Controls

| Key | Action |
| --- | --- |
| Arrows / WASD | Accelerate, brake and steer |
| Space + steer | Drift; releasing Space fires the charged boost |
| P | Pause / resume |
| Enter | Resume a pause, or start another race after the finish |
| R | Return to the last safe road position, losing speed and boost |
| Q | Switch between high quality and fast mode |
| Esc | Quit |

The drift charge turns blue after 40 simulation steps and gold after 90;
releasing Space then gives 45 or 90 steps of boost. There are 60 steps a
second. Leaving the track for the grass cancels the charge. Pressing the
throttle between 8 and 30 steps before the start gives a launch boost; holding
it through the whole countdown does not.

A lap counts when the three checkpoints are crossed in order and then the line
in the racing direction. The position tells the approach to the start from the
end of a lap. At the finish the time and the result freeze. Lap times are kept
for the race. Losing the focus pauses the game. Barriers contain the kart;
after one second continuously off the road it returns to its last safe point.
Release a held steering key before steering again after recovery. Off-road
movement cannot advance checkpoints. The scene remains visible in every direction,
including when facing backwards, and queued key releases are drained each frame.

## Graphics

- High: native rendering at 960×540. Fast: 480×270 doubled.
- The interface at 960×540 in both modes: position, speed, map, timer, best
  lap, drift charge and result.
- Reciprocal depth per pixel and near-plane clipping: objects and slopes hide
  each other correctly.
- Procedural asphalt and grass textures with perspective coordinates, the
  detail fading with distance.
- A warm sky, sun halo, clouds and three layers of mountains; trees, rocks,
  stands, chalets, waving flags, signs and barriers.
- Sculpted kart bodywork and helmets with smooth, perspective-correct lighting,
  clear-coat highlights, sky reflections, 16-sided tyres and metallic hubs.
- Blended contact shadows, soft procedural clouds, shaded snowy ridges, varied
  vegetation, boost flames and drift sparks.
- Bloom and colour finishing, applied before the interface.
- A chase camera that widens its field of view with speed.

Geometry rasterization runs on the CPU in integer arithmetic. On the Intel
Gen7 (Ivy Bridge) compute backend supported by AriCodeOS, Pista submits an EU
kernel for bloom and colour finishing. This is a hybrid renderer, **not a full
GPU 3D pipeline**, and does not add support for other GPU families.

The GPU path is enabled only after a hardware readback test matches the CPU
reference at every pixel, including image edges. An unavailable/busy device,
`gpu=off`, a failed check or dispatch error uses the CPU effect instead. Buffers
from a failed dispatch are not reused. The pause screen identifies the active
backend; the serial log also records detection and any fallback. QEMU verifies
the absent/disabled paths; successful compilation and host tests do not prove
execution on physical Gen7 hardware.

The app asks for 16 MiB, including its optional GPU input/output/state mappings.
Its speed inside AriCodeOS includes rendering, finishing, framebuffer copies
and the compositor; host example measurements exclude the compositor.

## Build and check

From the repository root:

```sh
cargo test -p aricode-pista --all-features
cargo clippy -p aricode-pista --all-features --all-targets -- -D warnings
cargo xtask app-pack desktop/pista --version 2
cargo xtask test --only gui.pista,gui.pista_controls
```

The package lands in `build/pkgs/pista-2.aripkg`. Scenario `gui.pista` installs
the app and presents 60 frames through the compositor; `gui.pista_controls`
drives the interactive path (quality changes, pause, recovery, driving and
quitting) with the compositor's keyboard. On a system with the package,
`run pista` opens it.

To render real captures and time the renderer on the host:

```sh
cargo run -p aricode-pista --release --example preview -- build/pista-preview
```

It writes PPM images of the grid and of two moments of a race, with no
AriCodeOS running and no window library. The benchmark inside AriCodeOS is still
`run pista 60`.
