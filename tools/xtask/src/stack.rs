// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Static stack budget of every initrd program (incident 0018).
//!
//! Every user process runs on a fixed 64 KiB stack with a guard page below
//! it (`kernel/src/user/process.rs`); a program that crosses it is killed
//! with `USER STACK OVERFLOW`. The network drivers' frames grew until a dev
//! build died on its first DHCP offer. This check reads the machine code of
//! every initrd program (`objdump -d`), measures every function's frame
//! from its prologue and the deepest chain of direct calls from `_start`,
//! and fails when either passes the program's budget, in the release and in
//! the dev build alike: by default half the stack for the path and a
//! quarter for one frame. A program allowed more is listed in
//! [`EXCEPTIONS`] with the reason and the incident that tracks it.
//!
//! The measured depth is a lower bound of the real one; what it does not
//! count:
//! - indirect calls (trait objects, `core::fmt` callbacks, function
//!   pointers): a path ends there;
//! - recursion, direct or mutual: a call back into a function already on
//!   the chain is cut (direct recursion is reported);
//! - `rsp` adjustments after a function's first call (dynamic `alloca`,
//!   stack arguments pushed later) and stack realignment (`and $-N,%rsp`),
//!   which add up to `N - 8` bytes;
//! - the red zone: since ADR 0065 user programs are built for a target
//!   that has one, so a leaf function may use 128 bytes below `rsp`
//!   without adjusting it.
//!
//! That is why the path budget is half the stack and not all of it. Direct
//! calls, unconditional tail calls (`jmp`) and conditional ones (`jcc` to
//! another function) are followed. Functions are keyed by address, so two
//! functions with the same demangled name are both measured.
//!
//! Part of `cargo xtask check`; alone as `cargo xtask stack-check`.
//! Started from the audit's `frames.py` and `depth.py` (incident 0018).

use crate::{build, root, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

/// The user stack of every process: `USER_STACK_SIZE` in
/// `kernel/src/user/process.rs` (a unit test keeps them equal).
pub const USER_STACK: u64 = 64 * 1024;

/// Stack allowed to one program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Deepest chain of direct calls.
    pub path: u64,
    /// Largest frame of one function.
    pub frame: u64,
}

/// Every program's budget unless [`EXCEPTIONS`] says otherwise: half the
/// stack for the path (the other half is for what the analysis cannot see
/// and for growth) and a quarter for one frame (a frame that size holds a
/// buffer or a structure that belongs on the heap).
pub const DEFAULT_BUDGET: Budget = Budget {
    path: USER_STACK / 2,
    frame: USER_STACK / 4,
};

/// Programs allowed more than [`DEFAULT_BUDGET`]: (binary, budget, reason
/// and tracking incident). Empty: every offender found when the check was
/// extended was fixed instead (incident 0018).
pub const EXCEPTIONS: &[(&str, Budget, &str)] = &[];

/// Budget of the initrd program `bin`.
pub fn budget(bin: &str) -> Budget {
    EXCEPTIONS
        .iter()
        .find(|(b, _, _)| *b == bin)
        .map_or(DEFAULT_BUDGET, |(_, budget, _)| *budget)
}

/// One function of the disassembly.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    /// Bytes it takes on the stack: the return address its caller pushed,
    /// its pushes and its `rsp` adjustment (stack probes included).
    pub frame: u64,
    /// Start addresses of the functions it calls or tail-calls directly,
    /// itself excluded.
    pub calls: BTreeSet<u64>,
    /// It calls itself directly (recursion, never followed).
    pub recursive: bool,
    /// Address of its last instruction.
    pub end: u64,
}

/// Functions by start address.
pub type Functions = BTreeMap<u64, Function>;

/// Every function of `objdump -d --no-show-raw-insn -C` output.
pub fn parse(disasm: &str) -> Functions {
    let mut funcs = Functions::new();
    // Branch targets as written (any address, function start or not).
    let mut targets: Vec<(u64, u64, bool)> = Vec::new();
    let mut current: Option<u64> = None;
    // Prologue state of the current function.
    let mut in_prologue = true;
    let mut rsp_in_r11 = false;
    let mut probe_loop = false;
    for line in disasm.lines() {
        if let Some((addr, name)) = header(line) {
            funcs.insert(
                addr,
                Function {
                    name: name.to_string(),
                    frame: 8,
                    end: addr,
                    ..Function::default()
                },
            );
            current = Some(addr);
            in_prologue = true;
            rsp_in_r11 = false;
            probe_loop = false;
            continue;
        }
        let Some(start) = current else {
            continue;
        };
        let Some((at, mnemonic, operands)) = instruction(line) else {
            continue;
        };
        let func = funcs.get_mut(&start).expect("current function");
        func.end = func.end.max(at);
        let is_call = mnemonic == "call";
        if is_call || mnemonic.starts_with('j') {
            if let Some(target) = direct_target(operands) {
                targets.push((start, target, is_call));
            }
        }
        if is_call {
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
    // A target belongs to the function that contains it (start at or below
    // it, last instruction at or above); a branch inside the same function
    // is a loop, not a call, and one outside every function is ignored.
    for (from, target, is_call) in targets {
        let Some((&to, _)) = funcs
            .range(..=target)
            .next_back()
            .filter(|(_, f)| f.end >= target)
        else {
            continue;
        };
        if to != from {
            funcs.get_mut(&from).expect("caller").calls.insert(to);
        } else if is_call && target == from {
            funcs.get_mut(&from).expect("caller").recursive = true;
        }
    }
    funcs
}

/// `0000000000400010 <name>:` -> `(0x400010, "name")`.
fn header(line: &str) -> Option<(u64, &str)> {
    let (addr, rest) = line.split_once(' ')?;
    let addr = hex(addr)?;
    Some((addr, rest.strip_prefix('<')?.strip_suffix(">:")?))
}

/// A non-empty hexadecimal number without prefix.
fn hex(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(s, 16).ok()
}

/// `  400010:\tpush   %rbp` -> `(0x400010, "push", "%rbp")`.
fn instruction(line: &str) -> Option<(u64, &str, &str)> {
    let (addr, text) = line.split_once(":\t")?;
    let addr = hex(addr.trim())?;
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
    Some((addr, mnemonic, rest))
}

/// `400100 <name+0x14>` -> `0x400100`; `None` for an indirect target
/// (`*%rax`).
fn direct_target(operands: &str) -> Option<u64> {
    let (addr, sym) = operands.split_once(' ')?;
    sym.starts_with('<').then_some(())?;
    hex(addr)
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

/// Start address of the function called `name` (the first, if several).
pub fn find(funcs: &Functions, name: &str) -> Option<u64> {
    funcs.iter().find(|(_, f)| f.name == name).map(|(&a, _)| a)
}

/// Deepest direct-call chain from the function at `root`. A call back into
/// a function already on the chain (recursion, direct or mutual) is not
/// followed: its depth is unbounded statically.
pub fn deepest(funcs: &Functions, root: u64) -> Option<CallPath> {
    fn walk(
        funcs: &Functions,
        addr: u64,
        on_chain: &mut Vec<u64>,
        memo: &mut BTreeMap<u64, CallPath>,
    ) -> CallPath {
        if let Some(p) = memo.get(&addr) {
            return p.clone();
        }
        let func = &funcs[&addr];
        on_chain.push(addr);
        let mut best: Option<CallPath> = None;
        for callee in &func.calls {
            if !funcs.contains_key(callee) || on_chain.contains(callee) {
                continue;
            }
            let p = walk(funcs, *callee, on_chain, memo);
            if best.as_ref().is_none_or(|b| p.total > b.total) {
                best = Some(p);
            }
        }
        on_chain.pop();
        let mut steps = vec![(func.name.clone(), func.frame)];
        let mut total = func.frame;
        if let Some(b) = best {
            total += b.total;
            steps.extend(b.steps);
        }
        let path = CallPath { total, steps };
        memo.insert(addr, path.clone());
        path
    }
    funcs.contains_key(&root).then(|| {
        let mut memo = BTreeMap::new();
        walk(funcs, root, &mut Vec::new(), &mut memo)
    })
}

/// Functions that call themselves directly (reported, never followed).
pub fn recursive(funcs: &Functions) -> Vec<&str> {
    funcs
        .values()
        .filter(|f| f.recursive)
        .map(|f| f.name.as_str())
        .collect()
}

/// The `n` largest frames, largest first.
pub fn largest(funcs: &Functions, n: usize) -> Vec<(&str, u64)> {
    let mut v: Vec<(&str, u64)> = funcs.values().map(|f| (f.name.as_str(), f.frame)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    v.truncate(n);
    v
}

/// Measurement of one binary against its budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub label: String,
    pub budget: Budget,
    pub path: CallPath,
    pub largest: Vec<(String, u64)>,
    /// Functions that call themselves (not followed).
    pub recursive: Vec<String>,
}

impl Verdict {
    pub fn ok(&self) -> bool {
        self.path.total <= self.budget.path && self.largest_frame() <= self.budget.frame
    }

    fn largest_frame(&self) -> u64 {
        self.largest.first().map_or(0, |(_, f)| *f)
    }

    /// One line; with the deepest path and the largest frames when over
    /// budget.
    pub fn report(&self) -> String {
        let (big_name, big) = self
            .largest
            .first()
            .map_or(("-", 0), |(n, f)| (n.as_str(), *f));
        let mut out = format!(
            "[xtask] stack {}: deepest direct path {} of {} bytes allowed ({USER_STACK} stack, {} left); largest frame {big} of {} ({big_name}){}\n",
            self.label,
            self.path.total,
            self.budget.path,
            USER_STACK.saturating_sub(self.path.total),
            self.budget.frame,
            if self.ok() { "" } else { ": OVER BUDGET" },
        );
        if !self.ok() {
            out.push_str("    deepest path:\n");
            for (name, frame) in &self.path.steps {
                out.push_str(&format!("      {frame:>6} {name}\n"));
            }
            out.push_str("    largest frames:\n");
            for (name, frame) in &self.largest {
                out.push_str(&format!("      {frame:>6} {name}\n"));
            }
            for name in &self.recursive {
                out.push_str(&format!("    note: {name} calls itself (not followed)\n"));
            }
        }
        out
    }
}

/// Measures one binary's functions against `budget`.
pub fn judge(label: &str, funcs: &Functions, budget: Budget) -> Result<Verdict> {
    let root = find(funcs, "_start").ok_or_else(|| format!("{label}: no _start"))?;
    let path = deepest(funcs, root).expect("root exists");
    Ok(Verdict {
        label: label.to_string(),
        budget,
        path,
        largest: largest(funcs, 5)
            .into_iter()
            .map(|(n, f)| (n.to_string(), f))
            .collect(),
        recursive: recursive(funcs).into_iter().map(String::from).collect(),
    })
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

/// Binaries always reported, in budget or not: the network drivers of the
/// incident.
const ALWAYS_REPORTED: [&str; 2] = ["virtio_net", "rtl_net"];

/// Builds every initrd program in the release and the dev profile and
/// judges each binary. Prints the network drivers, the three binaries
/// closest to their path budget and every one over budget.
pub fn check() -> Result {
    let programs = build::initrd_programs()?;
    for (bin, _, _) in EXCEPTIONS {
        if !programs.iter().any(|p| p == bin) {
            return Err(format!(
                "stack::EXCEPTIONS lists '{bin}', which is not an initrd program"
            ));
        }
    }
    let mut verdicts = Vec::new();
    for release in [true, false] {
        let profile = if release { "release" } else { "debug" };
        let mut c = build::user_cargo();
        c.args(["build", "--target", build::USER_TARGET]);
        for pkg in build::USER_PACKAGES {
            c.args(["-p", pkg]);
        }
        if release {
            c.arg("--release");
        }
        build::run(c)?;
        for bin in &programs {
            let elf = root()
                .join("target")
                .join(build::USER_TARGET)
                .join(profile)
                .join(bin);
            let funcs = parse(&disassemble(&elf)?);
            let label = format!("{bin} ({})", if release { "release" } else { "dev" });
            verdicts.push((bin.clone(), judge(&label, &funcs, budget(bin))?));
        }
    }
    let mut closest: Vec<&Verdict> = verdicts.iter().map(|(_, v)| v).collect();
    closest.sort_by(|a, b| (b.path.total * a.budget.path).cmp(&(a.path.total * b.budget.path)));
    let mut shown = BTreeSet::new();
    for (bin, v) in &verdicts {
        if ALWAYS_REPORTED.contains(&bin.as_str()) || !v.ok() {
            shown.insert(v.label.clone());
        }
    }
    for v in closest.iter().take(3) {
        shown.insert(v.label.clone());
    }
    for (_, v) in &verdicts {
        if shown.contains(&v.label) {
            print!("{}", v.report());
        }
    }
    let failed: Vec<&str> = verdicts
        .iter()
        .filter(|(_, v)| !v.ok())
        .map(|(_, v)| v.label.as_str())
        .collect();
    println!(
        "[xtask] stack: {} binaries measured, {} over budget",
        verdicts.len(),
        failed.len()
    );
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
  400204:\tje     4003f7 <dup+0x7>
  400206:\tret

0000000000400300 <tail>:
  400300:\tpush   %rbx
  400301:\tcall   400300 <tail>
  400306:\tcall   400999 <outside>
  40030b:\tpop    %rbx
  40030c:\tret

0000000000400380 <dup>:
  400380:\tsub    $0x10,%rsp
  400384:\tret

00000000004003f0 <dup>:
  4003f0:\tsub    $0x2000,%rsp
  4003f7:\tret
";

    fn at<'a>(f: &'a Functions, name: &str) -> &'a Function {
        &f[&find(f, name).unwrap()]
    }

    #[test]
    fn frames_count_pushes_probes_and_negative_adds() {
        let f = parse(SAMPLE);
        assert_eq!(at(&f, "_start").frame, 8 + 8);
        // Return address, two pushes, the probed 0xa000 and the rest; the
        // `sub` after the first call is not prologue.
        assert_eq!(at(&f, "big::run<x::Nic>").frame, 8 + 16 + 0xa000 + 0xf98);
        // Unrolled probes: every page counts.
        assert_eq!(at(&f, "<s::Stack<x::Nic>>::serve").frame, 8 + 8 + 0x2200);
        assert_eq!(at(&f, "leaf").frame, 8 + 128);
        assert_eq!(at(&f, "tail").frame, 16);
    }

    #[test]
    fn functions_with_the_same_name_are_both_kept() {
        let f = parse(SAMPLE);
        let dups: Vec<u64> = f
            .values()
            .filter(|x| x.name == "dup")
            .map(|x| x.frame)
            .collect();
        assert_eq!(dups, [8 + 0x10, 8 + 0x2000]);
        // The conditional tail call into the second `dup` (at an offset)
        // reaches that one, not the first.
        assert_eq!(
            at(&f, "leaf").calls.iter().copied().collect::<Vec<_>>(),
            [0x4003f0]
        );
    }

    #[test]
    fn calls_are_direct_and_tail_calls_count() {
        let f = parse(SAMPLE);
        let run = at(&f, "big::run<x::Nic>");
        let serve = find(&f, "<s::Stack<x::Nic>>::serve").unwrap();
        assert!(run.calls.contains(&serve));
        assert!(run.calls.contains(&find(&f, "leaf").unwrap()));
        // The probe loop's backward jump is not a call; `call *%rax` is
        // indirect and not followed.
        assert_eq!(run.calls.len(), 2);
        assert!(f[&serve].calls.contains(&find(&f, "tail").unwrap()));
        assert_eq!(recursive(&f), vec!["tail"]);
    }

    #[test]
    fn deepest_path_follows_the_heaviest_chain() {
        let f = parse(SAMPLE);
        let p = deepest(&f, find(&f, "_start").unwrap()).unwrap();
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
        assert!(deepest(&f, 0x1234).is_none());
    }

    #[test]
    fn mutual_recursion_is_cut() {
        let text = "\
0000000000400000 <_start>:
  400000:\tcall   400010 <a>
0000000000400010 <a>:
  400010:\tsub    $0x100,%rsp
  400017:\tcall   400020 <b>
0000000000400020 <b>:
  400020:\tsub    $0x200,%rsp
  400027:\tcall   400010 <a>
";
        let f = parse(text);
        let p = deepest(&f, find(&f, "_start").unwrap()).unwrap();
        assert_eq!(p.total, 8 + (8 + 0x100) + (8 + 0x200));
    }

    #[test]
    fn judge_fails_over_either_budget() {
        let f = parse(SAMPLE);
        let v = judge("x (release)", &f, DEFAULT_BUDGET).unwrap();
        assert!(!v.ok());
        assert!(v.report().contains("OVER BUDGET"));
        assert!(v.report().contains("big::run<x::Nic>"));
        assert!(v.report().contains("note: tail calls itself"));
        let mut small = f.clone();
        for name in ["big::run<x::Nic>", "<s::Stack<x::Nic>>::serve"] {
            small.get_mut(&find(&f, name).unwrap()).unwrap().frame = 4096;
        }
        let v = judge("x (release)", &small, DEFAULT_BUDGET).unwrap();
        assert!(v.ok(), "{}", v.report());
        // One frame over its budget fails even with a short path.
        small.get_mut(&find(&f, "leaf").unwrap()).unwrap().frame = DEFAULT_BUDGET.frame + 8;
        assert!(!judge("x", &small, DEFAULT_BUDGET).unwrap().ok());
        // A larger budget lets it through.
        let wide = Budget {
            path: USER_STACK,
            frame: USER_STACK,
        };
        assert!(judge("x", &small, wide).unwrap().ok());
        assert!(judge("x", &Functions::new(), wide).is_err());
    }

    #[test]
    fn exceptions_carry_a_reason_and_an_incident() {
        for (bin, b, why) in EXCEPTIONS {
            assert!(why.contains("incident"), "{bin}: no tracking incident");
            assert!(
                b.path > DEFAULT_BUDGET.path || b.frame > DEFAULT_BUDGET.frame,
                "{bin}: an exception must allow more than the default"
            );
            assert!(b.path < USER_STACK, "{bin}: budget over the stack");
        }
        assert_eq!(budget("virtio_net"), DEFAULT_BUDGET);
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
