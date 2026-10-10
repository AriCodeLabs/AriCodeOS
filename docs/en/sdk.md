# Application SDK (Phase 27)

English version of [docs/sdk.md](../sdk.md); the Spanish one is canonical.
Whoever changes one updates both in the same branch.

An AriCodeOS program is a static ring 3 ELF loaded at `0x400000` that can
only do what its handles allow. The SDK is a set of crates under `sdk/`:

| Crate | What it provides |
|-------|------------------|
| `aricode-abi` (`sdk/abi`) | syscall numbers, errors, object kinds, rights, initial handles |
| `aricode-sys` (`sdk/rust`) | syscall wrappers and service clients: `fs`, `net`, `http`, `input`, `window`, `block`, `time`, `permissions`, `virtio_pci` |
| `aricode-proto` (`sdk/proto`) | the versioned IPC protocols of every service |
| `aricode-rt` (`sdk/runtime`) | `entry!`, a 256 KiB heap, panic handler and formatting helpers; re-exports `aricode-sys` as `sys` |
| `aricode-build` (`sdk/build`) | one-line `build.rs` that links with `sdk/rust/user.ld` |

## Minimal program

`Cargo.toml`:

```toml
[package]
name = "my-app"
version.workspace = true
edition.workspace = true

[[bin]]
name = "my_app"
path = "src/main.rs"
test = false

[dependencies]
aricode-rt = { path = "../../sdk/runtime" }

[build-dependencies]
aricode-build = { path = "../../sdk/build" }
```

`build.rs`:

```rust
fn main() {
    aricode_build::user_program();
}
```

`src/main.rs`:

```rust
#![no_std]
#![no_main]

use aricode_rt::sys;

fn main(arg: u64) -> u64 {
    sys::print(b"hello\n");
    0 // exit code
}

aricode_rt::entry!(main);
```

To get it into the initrd, add the package to `Cargo.toml` (workspace), to
`PACKAGES` and to `build_user_programs`/`initrd_programs` in
`tools/xtask/src/build.rs`. `apps/hello` is the complete example.

## Out-of-tree applications (stage 4, ADR 0017)

An application does not have to live in the workspace or in the initrd.
`examples/hello_ext` is a crate with its own `[workspace]`, the SDK by path
(`aricode-rt = { path = "../../sdk/runtime" }`) and a `NAME.manifest` next
to it:

```bash
cargo xtask app-pack examples/hello_ext --version 1   # build/pkgs/hello_ext-1.aripkg
```

It is built from the repository root, so it uses the same
`rust-toolchain.toml` and `.cargo/config.toml` as the initrd programs, and it
receives the version in `APP_VERSION` at compile time. The package
(`aricode-pkg`) is a single file signed with the development key: package
manifest (name, version, SHA-256 of the program and of its manifest),
signature, ELF and application manifest. Installed in AriFS as
`apps/NAME.aripkg` (a scenario does it with `install = ["examples/hello_ext"]`),
the broker launches it with `run NAME` when it is not found in the initrd:
it verifies the signature and hashes, applies the same policy to the
manifest and creates the process from the buffer with `process_spawn_from`.
An installed program cannot ask for more than an initrd program with the
same manifest, and a package that does not verify is not launched. From
inside the system, `run installer` installs or updates the packages left in
`pkgs/` (ADR 0018): it verifies each one, installs only versions newer than
the installed one, and does so with a single atomic `replace`. A package
cannot exceed 32 KiB (the AriFS journal).

## What a program receives

Fixed handles (`aricode_abi::handle`): `0x10001` its process, `0x10002` its
main thread, `0x10003` the console (`WRITE`). Everything else is handed over
by whoever launches it, in order, from `0x10004` on, and the `arg` of `main`
is the value that launcher chose. There are no environment variables and no
service discovery: a program that does not receive the endpoint of a
service cannot talk to it. `sys::permissions::granted` walks what was
received.

## APIs

| Area | How |
|------|-----|
| process | `sys::process_spawn_with(spawner, name, arg, handles)`, `sys::process_wait`, `sys::process_kill` (`MANAGE` on the process; afterwards `process_wait` returns `Killed`), `sys::process_exit`; needs a `SPAWNER` handle with `MANAGE` (init has it; the shell gets one for `run`) |
| ipc | `sys::endpoint_create`, `sys::ipc_send`, `sys::ipc_receive` (256 bytes and up to 4 handles per message, with a deadline), `sys::ipc_receive_any` (the first of up to 16 endpoints with a message, with its index; ADR 0020); large data goes through shared DMA buffers |
| filesystem | `sys::fs::FsClient` over the `fsd` endpoint and a `HARDWARE` capability for its buffer: `list`, `open`, `create`, `read`, `write` (in place, durable after `flush`), `replace` (all-or-nothing and durable, up to 32 KiB: in chunks to the journal above 4 KiB), `flush`, `close` (ADR 0014, 0018) |
| network | `sys::net::NetClient`: `info`, `link` (physical link state, even before DHCP), `ping`, UDP (`udp_bind/send/recv`), TCP (`tcp_connect/listen/accept/send/recv/close`), `resolve`; `sys::http::get` on top |
| window | `sys::window::Window::create(compositor, hardware, w, h, title)`, `canvas()` (an `aricode-gfx` canvas), `present`, `poll` (`Key`, `Click`, `Focus`), `close` |
| input | `sys::input::InputClient` over an `inputd`: evdev events with timestamps; `aricode-input::Keyboard` translates them |
| console | `sys::console_write` (`WRITE`), `sys::console_read` (`READ`, with a deadline); `sys::console_inject` requires `MANAGE` and is only used by init's console bridge (ADR 0013) |
| time | `sys::time::uptime_ms(system)`, `sys::time::uptime_us(system)` (calibrated TSC), `sys::time::Clock`, `sys::time::sleep_ms` |
| permissions | `sys::permissions::{granted, kind_name, is, drop_handle}` |

## Manifest and launch (Phase 28)

An application is launched through the broker (init) with `run NAME [ARG]`
from the shell or `sys::launch::run(broker, name, arg)`; `spawn` and
`sys::launch::spawn` launch it in the background without waiting for its
exit. The broker allows 4 running programs at a time and answers `Limit` to
the fifth (ADR 0015). The broker reads `NAME.manifest` from the initrd
(`apps/<app>/NAME.manifest`, packed by xtask):

```text
name = hello_app
caps = system, fs      # system | fs | net | window | input | block (updater only) | install (installer only)
memory = 128K          # DMA buffer quota (K or M, at most 8M)
read_files = hola.txt  # up to 4 AriFS files in read-only mode (ADR 0016)
```

Handles arrive in this fixed order, only the requested ones: `SYSTEM`
(read), the fsd private directory, one endpoint per `read_files` entry (in
order; from it the program can only list, open and read that file, and
everything else answers `Denied`), the network endpoint, the compositor
endpoint, the block endpoint, the fsd main namespace (`install`), the input
endpoints and, if any capability needs buffers (`fs`, `read_files`, `net`,
`window`, `block`, `install`), `HARDWARE` at the end. With
`sys::permissions::granted` the application recognises them by kind without
depending on the position. Without a manifest only the console and `SYSTEM`
are received. Asking for `spawner`, `pci` or `framebuffer` makes the broker
deny the launch.

## Rules

- Handing a handle to another process requires `TRANSFER` on it, and making
  a copy with fewer rights requires `DUPLICATE`. The launcher decides which
  rights travel; they can never be widened (`Error::Escalation`).
- No `alloc` in the SDK libraries; the program has a static 256 KiB heap if
  it needs one (`extern crate alloc`).
- Every kernel error arrives as `sys::Error`; services answer with their own
  `Status` (`aricode-proto`). Names are never truncated: if they do not fit,
  the call fails.
- A program that does something illegal (touching kernel memory, privileged
  instructions, stack overflow) dies alone; the kernel and everything else
  carry on.

## Deadlines and correlation of calls (ADR 0012)

Every call of the SDK clients (`block`, `fs`, `net`, `window`, `input`,
`launch`, `status`) goes through `aricode_sys::rpc::call`:

- each request creates its own reply endpoint and closes it on return; a
  late reply is lost and is never attributed to another request;
- send and receive have deadlines (the one requested plus `rpc::GRACE_MS`);
  a dead service produces `Error::Timeout`, not an infinite wait. Only
  `launch::run` waits without a deadline for the exit of the launched
  program;
- an expired deadline does not undo the operation: its result is unknown.
  The SDK does not retry writes; the program decides. Reads and side-effect
  free queries can be repeated.
