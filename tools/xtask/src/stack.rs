// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Static stack budget of the network drivers (incident 0018).
//!
//! Every user process runs on a fixed 64 KiB stack with a guard page below
//! it (`kernel/src/user/process.rs`); a driver that crosses it is killed
//! with `USER STACK OVERFLOW`. The network drivers carry the whole stack of
//! `ari-net-core`, whose frames grew until the deepest path of a dev build
//! no longer fitted. This check reads the drivers' machine code
//! (`objdump -d`), measures every function's frame from its prologue and
//! the deepest chain of direct calls from `_start`, and fails when either
//! passes its budget, in the release and in the dev build alike: half the
//! stack for the path, a quarter for one frame.
//!
//! What it does not see: indirect calls (trait objects, `core::fmt`
//! callbacks, function pointers) end a path, so the measured depth is a
//! lower bound of the real one. That is why the path budget is half the
//! stack and not all of it.
//!
//! Started from the audit's `frames.py` and `depth.py` (incident 0018).

use crate::{build, root, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

/// The user stack of every process: `USER_STACK_SIZE` in
/// `kernel/src/user/process.rs` (a unit test keeps them equal).
pub const USER_STACK: u64 = 64 * 1024;
/// Deepest chain of direct calls allowed: half the stack. The other half is
/// for what the analysis cannot see (indirect calls) and for growth.
pub const PATH_BUDGET: u64 = USER_STACK / 2;
/// Largest frame allowed for one function: a quarter of the stack. A frame
/// this size holds a buffer or a structure that belongs on the heap.
pub const FRAME_BUDGET: u64 = USER_STACK / 4;

/// Packages and binaries under budget: the network drivers (ADR 0024).
pub const CHECKED: [(&str, &str); 2] =
    [("ari-virtio-net", "virtio_net"), ("ari-rtl-net", "rtl_net")];

/// One function of the disassembly.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Function {
    /// Bytes it takes on the stack: the return address its caller pushed,
    /// its pushes and its `rsp` adjustment (stack probes included).
    pub frame: u64,
    /// Functions it calls or tail-calls directly, itself excluded.
    pub calls: BTreeSet<String>,
    /// It calls itself directly (recursion, never followed).
    pub recursive: bool,
}

/// Every function of `objdump -d --no-show-raw-insn -C` output, by name.
pub fn parse(disasm: &str) -> BTreeMap<String, Function> {
    let mut funcs = BTreeMap::new();
    let mut current: Option<(String, Function)> = None;
    // Prologue state of the current function.
    let mut in_prologue = true;
    let mut rsp_in_r11 = false;
    let mut probe_loop = false;
    for line in disasm.lines() {
        if let Some(name) = header(line) {
            if let Some((n, f)) = current.take() {
                funcs.insert(n, f);
            }
            current = Some((
                name.to_string(),
                Function {
                    frame: 8,
                    ..Function::default()
                },
            ));
            in_prologue = true;
            rsp_in_r11 = false;
            probe_loop = false;
            continue;
        }
        let Some((name, func)) = current.as_mut() else {
            continue;
        };
        let Some((mnemonic, operands)) = instruction(line) else {
            continue;
        };
        if mnemonic == "call" || mnemonic == "jmp" {
            if let Some(target) = direct_target(operands) {
                if target != name.as_str() {
                    func.calls.insert(target.to_string());
                } else if mnemonic == "call" {
                    func.recursive = true;
                }
            }
        }
        if mnemonic == "call" {
            // The frame is complete before the first call.
            in_prologue = false;
        }
        if !in_prologue {
            continue;
        }
        match mnemonic {
            "push" => func.frame += 8,
            "mov" if operands == "%rsp,%r11" => rsp_in_r11 = true,
            "sub" => match immediate(operands) {
                // Stack probe loop (frames of a page or more): `r11` marks
                // the end, the loop moves `rsp` one page at a time.
                Some((v, "%r11")) if rsp_in_r11 => {
                    func.frame += v;
                    probe_loop = true;
                }
                Some((0x1000, "%rsp")) if probe_loop => {}
                Some((v, "%rsp")) => func.frame += v,
                _ => {}
            },
            // LLVM writes `sub $128` as `add $-128` (shorter immediate).
            "add" => {
                if let Some((v, "%rsp")) = immediate(operands) {
                    let v = v as i64;
                    if v < 0 {
                        func.frame += v.unsigned_abs();
                    }
                }
            }
            _ => {}
        }
    }
    if let Some((n, f)) = current {
        funcs.insert(n, f);
    }
    funcs
}

/// `0000000000400010 <name>:` -> `name`.
fn header(line: &str) -> Option<&str> {
    let (addr, rest) = line.split_once(' ')?;
    if addr.is_empty() || !addr.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    rest.strip_prefix('<')?.strip_suffix(">:")
}

/// `  400010:\tpush   %rbp` -> `("push", "%rbp")`.
fn instruction(line: &str) -> Option<(&str, &str)> {
    let (addr, text) = line.split_once(":\t")?;
    if !addr.trim().bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let text = text.trim();
    // Prefixes (`bnd jmp`, `notrack call`, `rep stos`) go before the mnemonic.
    let mut words = text.split_whitespace();
    let mut mnemonic = words.next()?;
    let mut rest = text[mnemonic.len()..].trim_start();
    if matches!(
        mnemonic,
        "bnd" | "notrack" | "rep" | "repz" | "repnz" | "lock"
    ) {
        mnemonic = words.next()?;
        rest = rest[mnemonic.len()..].trim_start();
    }
    let mnemonic = mnemonic.trim_end_matches('q');
    Some((mnemonic, rest))
}

/// `400100 <name+0x14>` -> `name`; `None` for an indirect target (`*%rax`).
fn direct_target(operands: &str) -> Option<&str> {
    let (addr, sym) = operands.split_once(' ')?;
    if !addr.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let sym = sym.strip_prefix('<')?.strip_suffix('>')?;
    Some(match sym.rfind("+0x") {
        Some(i) if sym[i + 3..].bytes().all(|b| b.is_ascii_hexdigit()) => &sym[..i],
        _ => sym,
    })
}

/// `$0x1000,%rsp` -> `(0x1000, "%rsp")`.
fn immediate(operands: &str) -> Option<(u64, &str)> {
    let (imm, reg) = operands.split_once(',')?;
    let hex = imm.strip_prefix("$0x")?;
    Some((u64::from_str_radix(hex, 16).ok()?, reg))
}

/// The deepest chain of direct calls from a root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallPath {
    /// Sum of the frames along the chain.
    pub total: u64,
    /// The functions, root first, with their frames.
    pub steps: Vec<(String, u64)>,
}

/// Deepest direct-call chain from `root`. A call back into a function
/// already on the chain (recursion) is not followed: its depth would be
/// unbounded statically, and none of the checked binaries recurse on the
/// paths that matter (`recursive` lists them for the report).
pub fn deepest(funcs: &BTreeMap<String, Function>, root: &str) -> Option<CallPath> {
    fn walk<'a>(
        funcs: &'a BTreeMap<String, Function>,
        name: &'a str,
        on_chain: &mut Vec<&'a str>,
        memo: &mut BTreeMap<&'a str, CallPath>,
    ) -> CallPath {
        if let Some(p) = memo.get(name) {
            return p.clone();
        }
        let func = &funcs[name];
        on_chain.push(name);
        let mut best: Option<CallPath> = None;
        for callee in &func.calls {
            if !funcs.contains_key(callee) || on_chain.contains(&callee.as_str()) {
                continue;
            }
            let p = walk(funcs, callee, on_chain, memo);
            if best.as_ref().is_none_or(|b| p.total > b.total) {
                best = Some(p);
            }
        }
        on_chain.pop();
        let mut steps = vec![(name.to_string(), func.frame)];
        let mut total = func.frame;
        if let Some(b) = best {
            total += b.total;
            steps.extend(b.steps);
        }
        let path = CallPath { total, steps };
        memo.insert(name, path.clone());
        path
    }
    funcs.contains_key(root).then(|| {
        let mut memo = BTreeMap::new();
        walk(funcs, root, &mut Vec::new(), &mut memo)
    })
}

/// Functions that call themselves directly (reported, never followed).
pub fn recursive(funcs: &BTreeMap<String, Function>) -> Vec<&str> {
    funcs
        .iter()
        .filter(|(_, f)| f.recursive)
        .map(|(n, _)| n.as_str())
        .collect()
}

/// The `n` largest frames, largest first.
pub fn largest(funcs: &BTreeMap<String, Function>, n: usize) -> Vec<(&str, u64)> {
    let mut v: Vec<(&str, u64)> = funcs.iter().map(|(k, f)| (k.as_str(), f.frame)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    v.truncate(n);
    v
}

/// Verdict for one binary: the report lines and whether it is in budget.
pub fn judge(label: &str, funcs: &BTreeMap<String, Function>) -> (String, bool) {
    let mut out = String::new();
    let Some(path) = deepest(funcs, "_start") else {
        return (
            format!("[xtask] stack {label}: no _start in the disassembly\n"),
            false,
        );
    };
    let top = largest(funcs, 5);
    let (big_name, big) = top.first().copied().unwrap_or(("-", 0));
    let ok = path.total <= PATH_BUDGET && big <= FRAME_BUDGET;
    out.push_str(&format!(
        "[xtask] stack {label}: deepest direct path {} of {PATH_BUDGET} bytes allowed ({USER_STACK} stack, {} left); largest frame {big} of {FRAME_BUDGET} ({big_name}){}\n",
        path.total,
        USER_STACK.saturating_sub(path.total),
        if ok { "" } else { ": OVER BUDGET" },
    ));
    if !ok {
        out.push_str("    deepest path:\n");
        for (name, frame) in &path.steps {
            out.push_str(&format!("      {frame:>6} {name}\n"));
        }
        out.push_str("    largest frames:\n");
        for (name, frame) in &top {
            out.push_str(&format!("      {frame:>6} {name}\n"));
        }
    }
    (out, ok)
}

/// Disassembles `elf` with binutils' `objdump`.
pub fn disassemble(elf: &Path) -> Result<String> {
    let out = Command::new("objdump")
        .args(["-d", "--no-show-raw-insn", "-C"])
        .arg(elf)
        .output()
        .map_err(|e| format!("objdump (binutils) not found: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "objdump {}: {}",
            elf.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("objdump output: {e}"))
}

/// Builds the checked drivers in the release and the dev profile and
/// judges each binary.
pub fn check() -> Result {
    let mut failed = Vec::new();
    for release in [true, false] {
        let profile = if release { "release" } else { "debug" };
        let mut c = build::cargo();
        c.args(["build", "--target", build::KERNEL_TARGET]);
        for (pkg, _) in CHECKED {
            c.args(["-p", pkg]);
        }
        if release {
            c.arg("--release");
        }
        build::run(c)?;
        for (_, bin) in CHECKED {
            let elf = root()
                .join("target")
                .join(build::KERNEL_TARGET)
                .join(profile)
                .join(bin);
            let funcs = parse(&disassemble(&elf)?);
            let label = format!("{bin} ({})", if release { "release" } else { "dev" });
            let (report, ok) = judge(&label, &funcs);
            print!("{report}");
            for r in recursive(&funcs) {
                println!("    note: {r} calls itself (not followed)");
            }
            if !ok {
                failed.push(label);
            }
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "stack budget exceeded (incident 0018, tests/README.md): {}",
            failed.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
target/x86_64-unknown-none/release/x:     file format elf64-x86-64


Disassembly of section .text:

0000000000400000 <_start>:
  400000:\tpush   %rax
  400001:\tcall   400010 <big::run<x::Nic>>
  400006:\tud2

0000000000400010 <big::run<x::Nic>>:
  400010:\tpush   %rbp
  400011:\tpush   %rbx
  40001a:\tmov    %rsp,%r11
  40001d:\tsub    $0xa000,%r11
  400024:\tsub    $0x1000,%rsp
  40002b:\tmovq   $0x0,(%rsp)
  400033:\tcmp    %r11,%rsp
  400036:\tjne    400024 <big::run<x::Nic>+0x14>
  400038:\tsub    $0xf98,%rsp
  40003f:\tcall   *%rax
  400041:\tcall   400100 <<s::Stack<x::Nic>>::serve>
  400046:\tcall   400200 <leaf>
  40004b:\tsub    $0x10000,%rsp
  400050:\tret

0000000000400100 <<s::Stack<x::Nic>>::serve>:
  400100:\tpush   %r15
  400102:\tsub    $0x1000,%rsp
  400109:\tmovq   $0x0,(%rsp)
  400110:\tsub    $0x1000,%rsp
  400117:\tmovq   $0x0,(%rsp)
  40011e:\tsub    $0x200,%rsp
  400125:\tjmp    400300 <tail>

0000000000400200 <leaf>:
  400200:\tadd    $0xffffffffffffff80,%rsp
  400204:\tret

0000000000400300 <tail>:
  400300:\tpush   %rbx
  400301:\tcall   400300 <tail>
  400306:\tcall   400999 <outside>
  40030b:\tpop    %rbx
  40030c:\tret
";

    #[test]
    fn frames_count_pushes_probes_and_negative_adds() {
        let f = parse(SAMPLE);
        assert_eq!(f["_start"].frame, 8 + 8);
        // Return address, two pushes, the probed 0xa000 and the rest; the
        // `sub` after the first call is not prologue.
        assert_eq!(f["big::run<x::Nic>"].frame, 8 + 16 + 0xa000 + 0xf98);
        // Unrolled probes: every page counts.
        assert_eq!(f["<s::Stack<x::Nic>>::serve"].frame, 8 + 8 + 0x2200);
        assert_eq!(f["leaf"].frame, 8 + 128);
        assert_eq!(f["tail"].frame, 16);
    }

    #[test]
    fn calls_are_direct_and_tail_calls_count() {
        let f = parse(SAMPLE);
        let run = &f["big::run<x::Nic>"];
        assert!(run.calls.contains("<s::Stack<x::Nic>>::serve"));
        assert!(run.calls.contains("leaf"));
        // The probe loop's backward jump is not a call; `call *%rax` is
        // indirect and not followed.
        assert_eq!(run.calls.len(), 2);
        assert!(f["<s::Stack<x::Nic>>::serve"].calls.contains("tail"));
        assert_eq!(recursive(&f), vec!["tail"]);
    }

    #[test]
    fn deepest_path_follows_the_heaviest_chain() {
        let f = parse(SAMPLE);
        let p = deepest(&f, "_start").unwrap();
        let names: Vec<&str> = p.steps.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "_start",
                "big::run<x::Nic>",
                "<s::Stack<x::Nic>>::serve",
                "tail"
            ]
        );
        assert_eq!(p.total, 16 + (24 + 0xa000 + 0xf98) + (16 + 0x2200) + 16);
        assert!(deepest(&f, "missing").is_none());
    }

    #[test]
    fn judge_fails_over_either_budget() {
        let f = parse(SAMPLE);
        let (report, ok) = judge("x (release)", &f);
        assert!(!ok);
        assert!(report.contains("OVER BUDGET"));
        assert!(report.contains("big::run<x::Nic>"));
        let mut small = f.clone();
        small.get_mut("big::run<x::Nic>").unwrap().frame = 4096;
        small.get_mut("<s::Stack<x::Nic>>::serve").unwrap().frame = 4096;
        let (report, ok) = judge("x (release)", &small);
        assert!(ok, "{report}");
        // One frame over its budget fails even with a short path.
        small.get_mut("leaf").unwrap().frame = FRAME_BUDGET + 8;
        assert!(!judge("x", &small).1);
    }

    #[test]
    fn budget_matches_the_kernel_user_stack() {
        let src = std::fs::read_to_string(root().join("kernel/src/user/process.rs"))
            .expect("read process.rs");
        assert!(
            src.contains("pub const USER_STACK_SIZE: u64 = 64 * 1024;"),
            "USER_STACK_SIZE changed: update stack::USER_STACK and the budgets"
        );
        assert_eq!(USER_STACK, 64 * 1024);
    }
}
