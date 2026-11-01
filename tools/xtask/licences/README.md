# Licence texts kept for the third-party notices

Some crates.io packages do not include their licence files; their
repositories keep them at the root. `cargo xtask notices` takes these
copies for them (`tools/xtask/src/notices.rs`, `FROM_REPOSITORY`):

| Directory | Crates | Source |
|---|---|---|
| `wasmi/` | wasmi, wasmi_core, wasmi_ir, wasmi_collections 2.0.0 | https://github.com/wasmi-labs/wasmi, tag `v2.0.0` |
| `wasm-tools/` | wasmparser 0.228.0 | https://github.com/bytecodealliance/wasm-tools, tag `v1.228.0` |

Refresh them when the crate's version changes.
