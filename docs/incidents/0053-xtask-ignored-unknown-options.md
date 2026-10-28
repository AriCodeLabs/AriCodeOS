# Incident 0053: xtask ignored options it did not know

- Opened: 2026-10-28. Status: fixed on `fix/xtask-unknown-options`.
- Found by: the work on incident 0052. `cargo xtask test --all --only
  update.withdrawn_image`, meant to run one scenario, ran all 203.

## Evidence

The log `/home/serverbig/aricode-logs/0052/repro.log` shows the full
suite: `203 QEMU scenarios, 1 failed`. `--only` does not exist, and
nothing said so.

## Mechanism

xtask's `Cli` looked options up by name when a command asked for them,
and never checked what was left over. Reading the code turned up three
more defects of the same kind:

- **Unknown options and extra arguments.** These were dropped silently,
  in every command.
- **Option values taken as positional arguments.** `positional()`
  skipped words starting with `--` but not their values, so
  `app-pack --version 3 DIR` packed directory `3`.
- **Invalid numbers replaced by the default.**
  - `run --cpus x` booted with one CPU.
  - `serve-update --port x` served on 8080.
  - `app-pack --version x` packed version 1.
- **A missing value read the next option.** `test --area --all` used
  `--all` as the area list.

## Fix

Each command declares what it takes: its options, whether each takes no
value, one value given once, or a value that may repeat (`--app`), and
how many positional arguments it allows. `Cli::parse` checks the
arguments against that before the command runs, and refuses each of
these with the reason (exit 2):

- an unknown option;
- an extra argument;
- an option without its value (a following `--option` does not count as
  one);
- a non-repeating option given twice.

Option values never count as positional arguments. Numbers go through
`Cli::number` and `Cli::cpus`. They refuse what does not parse instead of
falling back to a default. `test`, `perf` and `run` share the 1 to 64
range for `--cpus`.

## Verification

- Host tests in `tools/xtask/src/main.rs`, module `tests`:
  - unknown options, `--x=y` and `-v` are refused;
  - option values are not positional, and extra arguments are refused;
  - values are required and given once, and `--app` repeats;
  - invalid ports and CPU counts are refused;
  - the invocations the documentation and scripts use still parse.
- Run by hand, these are refused with the reason:
  - `cargo xtask test --only x`;
  - `cargo xtask check --debug`;
  - `cargo xtask run --cpus 0`.
- `cargo xtask check` and `cargo xtask test --level unit` pass.
- Every `cargo xtask` invocation in the repository's scripts and
  documentation was checked against the new specs.
