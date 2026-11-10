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
| R | Back to the current stretch, losing speed and boost |
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
for the race. Losing the focus pauses the game.

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
- Karts with octagonal tyres, steering front wheels, a faceted helmet, a spoiler,
  boost flames and drift sparks.
- A chase camera that widens its field of view with speed.

Rendering runs on the CPU in integer arithmetic, with no outside graphics
dependency. The app asks for 16 MiB: the window, the colour and depth buffers
and the runtime. Its speed inside AriCodeOS also includes the framebuffer copy
and the compositor; the host example's figures are not the system's whole frame
rate.

## Build and check

From the repository root:

```sh
cargo test -p aricode-pista
cargo clippy -p aricode-pista --all-targets -- -D warnings
cargo xtask app-pack desktop/pista
cargo xtask test --only gui.pista,gui.pista_controls
```

The package lands in `build/pkgs/pista-1.aripkg`. Scenario `gui.pista` installs
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
