# Evidence for the 6ee19fb review

Commit: `6ee19fb2b5d64887d5d9ccba3215641727ef7fe9`. Date: 2026-09-29.
Report: [supervision, persistence, packages and WASM](../2026-09-29-services-persistence-packages-wasm-6ee19fb.md).

## Files

| File | Contents |
| --- | --- |
| `check.log` | `cargo xtask check` on the commit copy. |
| `tests.log` | Full official suite; also contains host results. |
| `host/src/lib.rs`, `host/Cargo.toml` | Seven additional probes against the original libraries. |
| `host-probes.log` | Output of the seven assertions; they fail because they require the correct behaviour. |
| `reference.log`, `*.wasm` | Comparison with `WebAssembly.validate` and loop execution in Node. |
| `app/` | Test application packaged twice, as `reviewa` and `reviewb`, with `caps = fs`. |
| `app-build.log`, `prepare.log`, `prepare.rs` | App build and package/disk preparation. |
| `journal-wasm-serial.log`, `journal-wasm-command.json` | QEMU session: journal leftovers, import with incompatible signature and abandoned transaction. |
| `supervisor-timeout-serial.log`, `supervisor-timeout-command.json` | QEMU session: service still alive after its probe times out. |
| `run-qemu.py`, `prepare-supervisor.py`, `run-supervisor.py` | Harness and fixture preparation used, with this review's concrete paths. |

No disk images, ELF binaries or build directories are stored in
the repository. The `.wasm` modules are minimal fixtures with synthetic data.
The probes do not modify production sources.

## Isolated official suite

The commit was exported with `git archive` to
`/var/tmp/aricode-review-6ee19fb-uhBqbF`. In `.rs`, `.toml` and `.md` files
under `sdk`, `tests`, `tools`, `apps` and `user`, the numbers
17007, 17008, 17009 and 17010 were replaced with 31007, 31008, 31009 and 31010, respectively.
This includes the HTTP URL and the expected network texts.

From that copy:

```bash
cargo xtask check > review-check.log 2>&1
cargo xtask test --all > review-tests.log 2>&1
```

The suite uses the CPUs declared by each scenario; the whole
suite was not forced to four CPUs. The concurrent tests in the original tree were not
interrupted and their results are not mixed with those of this audit.

## Host probes

From a copy of the repository at the reviewed commit that includes this evidence
folder:

```bash
CARGO_TARGET_DIR=/var/tmp/aricode-review-host-target cargo test \
  --manifest-path docs/audits/6ee19fb-review/host/Cargo.toml -- --nocapture
```

Expected result on `6ee19fb`: seven failing tests (Cargo exit code 101).
They check the desired behaviour; no test is written that treats the defect
as a success. When run, Cargo may create `Cargo.lock` and the four WASM fixtures
in the probe project directory.

The simulated disk distinguishes pending and persisted sectors. `flush` makes
all previous writes durable. A power cut keeps only the pending table
sectors and discards the rest; it keeps whole sectors, never
torn ones. This is an execution allowed by the `BlockDevice` contract, which
does not guarantee ordering before `flush`.

The original probes were run in `/var/tmp/aricode-probes-6ee19fb`, with
path dependencies on the `6ee19fb` copy. The archived manifests use
paths relative to the repository to make them easier to rerun.

Independent comparison, from the repository root:

```bash
node - <<'JS'
const fs = require('fs');
const root = 'docs/audits/6ee19fb-review/';
console.log('Node', process.version);
for (const file of ['wrong-global.wasm', 'missing-memory.wasm',
                    'inverted-limits.wasm', 'loop-params.wasm',
                    'wrong-import.wasm']) {
  const bytes = fs.readFileSync(root + file);
  console.log(file, 'valid=', WebAssembly.validate(bytes));
  if (file === 'loop-params.wasm') {
    const instance = new WebAssembly.Instance(new WebAssembly.Module(bytes));
    console.log('reference result=', instance.exports.main());
  }
}
JS
```

The first three must be rejected; the loop returns 0. `wrong-import.wasm`
is structurally valid: its incompatibility arises when linking it against
the signature of `aricode.file_write`, not when validating its standalone structure.

## QEMU reproductions

The scripts keep the original paths. For another directory, replace
`/var/tmp/aricode-review-6ee19fb-uhBqbF` and `/var/tmp/aricode-probes-6ee19fb`
in the scripts, temporary manifests and preparation tool. Use an isolated
copy: `prepare.rs` modifies the disk it is given and `prepare-supervisor.py`
modifies the initrd and slots of its own test copy.

Preparing the first session:

1. Build the commit and let `boot.kernel_ready` finish. The review
   took its `build/scenarios/boot.kernel_ready` as the starting disk/ESP.
2. Build `app/` for `x86_64-unknown-none`, with the SDK linker script:
   `RUSTFLAGS='-C relocation-model=static -C code-model=kernel' cargo build
   --manifest-path app/Cargo.toml --target x86_64-unknown-none --release`.
   Use `strip --strip-debug` to leave `review_app.elf` in the probe directory.
3. Copy the scenario to `qemu/`, write `qemu_exit shell` to
   `qemu/esp/aricodeos/cmdline.txt` and restore `OVMF_VARS.fd` from
   `/usr/share/OVMF/OVMF_VARS_4M.fd`.
4. Build `prepare.rs` as a host binary with path dependencies `aricode-arifs`,
   `aricode-wasm`, `aricode-crypto` and `aricode-pkg` (feature `write`) on
   the commit. Run it from the probe directory with `qemu/disk.img`.
   It uses only the repository's development key. It installs two
   packages of the test app and replaces `wasm_runner/demo.wasm` with the
   minimal incompatible-import module.
5. Run `run-qemu.py`. During this review the official suite was still running,
   and its test responders on 31007/31008/31010 also served the
   network self-tests of the additional guest. Outside that context those
   responders must be kept running; otherwise init may fail before
   reaching the shell. The additional guest does not open `hostfwd`.

The session sends, waiting for each prompt:

```text
run reviewa 1
run reviewb 2
run wasm_runner
run reviewa 3
run reviewb 4
shutdown
```

The first two commands reproduce the journal data crossover. The third
produces the runner panic. The next two test the occupied journal
after the owner of a transaction dies. QEMU exits with 33; that code
only confirms a normal system shutdown, not that the properties
being checked hold.

For the supervisor, run `prepare-supervisor.py` and then
`run-supervisor.py`, with the same responders running. The fixture only replaces
`virtio_input` with the `cpu_hog` binary already present in the initrd. It invalidates
the A/B state and the slot A header on its disk so recovery is used
from the modified ESP. The kernel and the supervisor code are not modified.
`services --detail`, `ps` and `shutdown` show the mismatch between the supervisor's
state and the live processes that keep the PCI devices.
