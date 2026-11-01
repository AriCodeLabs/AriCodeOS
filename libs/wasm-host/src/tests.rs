// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
extern crate std;

use super::*;
use std::format;
use std::string::String;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wasmi::{Linker, Module, Store, TrapCode};

struct State {
    limits: StoreLimits,
}

fn store(engine: &Engine) -> Store<State> {
    let mut s = Store::new(engine, State { limits: limits() });
    s.limiter(|s| &mut s.limits);
    s.set_fuel(FUEL).unwrap();
    s
}

fn module(engine: &Engine, text: &str) -> Result<Module, wasmi::Error> {
    Module::new(engine, &wat::parse_str(text).unwrap()[..])
}

fn run_main(text: &str) -> Result<i32, wasmi::Error> {
    let engine = engine();
    let m = module(&engine, text)?;
    let mut s = store(&engine);
    let i = Linker::new(&engine).instantiate_and_start(&mut s, &m)?;
    i.get_typed_func::<i64, i32>(&s, "main")?.call(&mut s, 0)
}

fn trap(r: Result<i32, wasmi::Error>) -> Option<TrapCode> {
    r.err().and_then(|e| e.as_trap_code())
}

/// Profile v2: floating point and multiple results run.
#[test]
fn floats_and_multi_value_run() {
    let r = run_main(
        r#"(module
            (func $pair (result f64 i32) (f64.const 1.5) (i32.const 2))
            (func (export "main") (param i64) (result i32)
                (call $pair) (f64.convert_i32_s) (f64.mul) (i32.trunc_f64_s)))"#,
    );
    assert_eq!(r.unwrap(), 3);
}

/// SIMD, exceptions and GC are outside the profile: refused when the module
/// is validated, before anything runs.
#[test]
fn simd_exceptions_and_gc_are_refused() {
    let engine = engine();
    for text in [
        r#"(module (func (result v128) (v128.const i32x4 0 0 0 0)))"#,
        r#"(module (tag $e) (func (throw $e)))"#,
        r#"(module (type (struct (field i32))))"#,
    ] {
        assert!(module(&engine, text).is_err(), "{text}");
    }
}

#[test]
fn an_endless_loop_runs_out_of_fuel() {
    let r = run_main(
        r#"(module (func (export "main") (param i64) (result i32) (loop (br 0)) (i32.const 0)))"#,
    );
    assert_eq!(trap(r), Some(TrapCode::OutOfFuel));
}

/// An import's price is charged before it runs: one the module cannot pay
/// for never runs (ADR 0055).
#[test]
fn an_import_it_cannot_pay_for_never_runs() {
    let engine = engine();
    let m = module(
        &engine,
        r#"(module (import "aricode" "print" (func $p (param i32 i32)))
            (func (export "main") (param i64) (result i32)
                (call $p (i32.const 0) (i32.const 0)) (i32.const 1)))"#,
    )
    .unwrap();
    let ran = Arc::new(AtomicBool::new(false));
    let seen = ran.clone();
    let mut linker = Linker::<State>::new(&engine);
    linker
        .func_wrap(
            "aricode",
            "print",
            move |mut c: Caller<'_, State>, _: i32, _: i32| {
                charge(&mut c, PRINT_COST)?;
                seen.store(true, Ordering::Relaxed);
                Ok(())
            },
        )
        .unwrap();
    let mut s = store(&engine);
    let i = linker.instantiate_and_start(&mut s, &m).unwrap();
    let main = i.get_typed_func::<i64, i32>(&s, "main").unwrap();
    s.set_fuel(PRINT_COST / 2).unwrap();
    let r = main.call(&mut s, 0);
    assert_eq!(trap(r), Some(TrapCode::OutOfFuel));
    assert!(!ran.load(Ordering::Relaxed), "the import's body never ran");
    // With the fuel for it, it runs.
    s.set_fuel(FUEL).unwrap();
    assert_eq!(main.call(&mut s, 0).unwrap(), 1);
    assert!(ran.load(Ordering::Relaxed));
}

/// `memory.grow` past `MEMORY_LIMIT` returns -1, as a failed grow does.
#[test]
fn memory_grows_to_the_limit_and_no_further() {
    let r = run_main(
        r#"(module (memory 1)
            (func (export "main") (param i64) (result i32)
                (drop (memory.grow (i32.const 15)))
                (memory.grow (i32.const 1))))"#,
    );
    assert_eq!(r.unwrap(), -1);
    let r = run_main(
        r#"(module (memory 1)
            (func (export "main") (param i64) (result i32) (memory.grow (i32.const 15))))"#,
    );
    assert_eq!(r.unwrap(), 1, "up to 16 pages (1 MiB)");
}

/// A module that asks for more than the limits at instantiation is not
/// instantiated: no memory or table is allocated for it.
#[test]
fn modules_over_the_limits_are_not_instantiated() {
    for text in [
        r#"(module (memory 17) (func (export "main") (param i64) (result i32) (i32.const 0)))"#,
        r#"(module (memory 1) (memory 1)
            (func (export "main") (param i64) (result i32) (i32.const 0)))"#,
        r#"(module (table 20000 funcref) (func (export "main") (param i64) (result i32) (i32.const 0)))"#,
    ] {
        assert!(run_main(text).is_err(), "{text}");
    }
}

#[test]
fn deep_recursion_is_a_stack_overflow_not_a_crash() {
    let r = run_main(
        r#"(module (func $f (param i32) (result i32) (call $f (local.get 0)))
            (func (export "main") (param i64) (result i32) (call $f (i32.const 0))))"#,
    );
    assert_eq!(trap(r), Some(TrapCode::StackOverflow));
}

/// Bulk operations pay one unit per 64 bytes (`WORK_BYTES`): filling the
/// whole 1 MiB costs at least 16 Ki units.
#[test]
fn bulk_memory_pays_for_its_bytes() {
    let engine = engine();
    let m = module(
        &engine,
        r#"(module (memory 16)
            (func (export "main") (param i64) (result i32)
                (memory.fill (i32.const 0) (i32.const 7) (i32.const 1048576)) (i32.const 0)))"#,
    )
    .unwrap();
    let mut s = store(&engine);
    let i = Linker::new(&engine)
        .instantiate_and_start(&mut s, &m)
        .unwrap();
    let main = i.get_typed_func::<i64, i32>(&s, "main").unwrap();
    // The first call also pays for translating the function.
    main.call(&mut s, 0).unwrap();
    s.set_fuel(FUEL).unwrap();
    main.call(&mut s, 0).unwrap();
    let spent = FUEL - s.get_fuel().unwrap();
    assert!(spent >= (1 << 20) / WORK_BYTES, "spent {spent}");
}

/// Audit of ADR 0071, H2: extended constant expressions are refused (wasmi
/// evaluated them recursively); a plain one still works.
#[test]
fn extended_constant_expressions_are_refused() {
    let engine = engine();
    let deep = format!(
        "(module (global i32 (i32.const 1) {}))",
        "(i32.const 1) i32.add ".repeat(2000)
    );
    assert!(module(&engine, &deep).is_err());
    assert!(module(&engine, "(module (global i32 (i32.const 1)))").is_ok());
}

/// Audit of ADR 0071, M1: nesting past `MAX_NESTING` is refused before
/// wasmi sees the module; at the limit it runs.
#[test]
fn deep_nesting_is_refused_before_translation() {
    let nested = |n: usize| {
        wat::parse_str(format!(
            "(module (func (export \"main\") (param i64) (result i32) {} (i32.const 7)))",
            "(block ".repeat(n) + &")".repeat(n)
        ))
        .unwrap()
    };
    assert_eq!(
        check_nesting(&nested(MAX_NESTING + 1)),
        Err("blocks nested deeper than 1024")
    );
    let ok = nested(MAX_NESTING);
    assert_eq!(check_nesting(&ok), Ok(()));
    let engine = engine();
    let m = Module::new(&engine, &ok[..]).unwrap();
    let mut s = store(&engine);
    let i = Linker::new(&engine)
        .instantiate_and_start(&mut s, &m)
        .unwrap();
    assert_eq!(
        i.get_typed_func::<i64, i32>(&s, "main")
            .unwrap()
            .call(&mut s, 0)
            .unwrap(),
        7
    );
    // Garbage is left to wasmi, which says why.
    assert_eq!(check_nesting(b"not wasm"), Ok(()));
}

/// Audit of ADR 0071, M2 and L1: wasmi's strict limits on what a module
/// contains, and a value stack that holds 1024 calls with a few locals.
#[test]
fn module_contents_and_stack_are_bounded() {
    let engine = engine();
    let many = format!("(module {})", "(func)".repeat(12_000));
    assert!(module(&engine, &many).is_err(), "too many functions");
    let globals = format!("(module {})", "(global i32 (i32.const 0))".repeat(1001));
    assert!(module(&engine, &globals).is_err(), "too many globals");
    let r = run_main(
        r#"(module
            (func $f (param i32) (result i32) (local i64 i64 i64 i64 i64 i64 i64 i64 i64 i64 i64 i64 i64 i64 i64 i64)
                (if (result i32) (local.get 0)
                    (then (call $f (i32.sub (local.get 0) (i32.const 1))))
                    (else (i32.const 9))))
            (func (export "main") (param i64) (result i32) (call $f (i32.const 1000))))"#,
    );
    assert_eq!(r.unwrap(), 9, "1000 nested calls with 16 locals each");
}

/// Audit of ADR 0071, L4: a module cannot write its own code or manifest.
#[test]
fn a_module_cannot_rewrite_itself() {
    assert!(!writable(MODULE_FILE) && !writable(MANIFEST_FILE));
    assert!(writable("notas"));
}

/// An invalid module is refused when it is loaded, with the reason.
#[test]
fn an_ill_typed_module_is_refused_before_it_runs() {
    let engine = engine();
    let e = module(
        &engine,
        r#"(module (func (result i32) (i32.add (i32.const 1))))"#,
    );
    assert!(e.is_err());
}

#[test]
fn manifest_capabilities() {
    assert_eq!(
        parse_caps("caps = print, files # comment\n"),
        Ok([true, false, true])
    );
    assert_eq!(parse_caps("# nothing\n\n"), Ok([false; 3]));
    assert_eq!(parse_caps("caps = net"), Err("unknown capability"));
    assert_eq!(parse_caps("memory = 1M"), Err("unknown key"));
    assert_eq!(parse_caps("caps print"), Err("line without '='"));
    let g = parse_caps("caps = clock").unwrap();
    assert!(granted(&g, Cap::Clock) && !granted(&g, Cap::Print));
}

#[test]
fn api_names_capabilities_and_signatures() {
    assert_eq!(Api::by_name("file_write"), Some(Api::FileWrite));
    assert_eq!(Api::by_name("exit"), None);
    assert_eq!(Api::UptimeMs.requires(), Cap::Clock);
    assert_eq!(Api::FileRead.requires(), Cap::Files);
    let print = FuncType::new([ValType::I32, ValType::I32], []);
    assert!(Api::Print.matches(&print));
    assert!(!Api::FileWrite.matches(&print));
    assert!(is_main(&FuncType::new([ValType::I64], [ValType::I32])));
    assert!(!is_main(&FuncType::new([ValType::I32], [ValType::I32])));
}

#[test]
fn file_names() {
    assert_eq!(file_name(b"notas"), Some("notas"));
    assert_eq!(file_name(b""), None);
    assert_eq!(file_name(b"a/b"), None);
    assert_eq!(file_name(&[0xFF]), None);
    assert_eq!(file_name(&[b'x'; MAX_FILE_NAME + 1]), None);
    assert!(file_name(&[b'x'; MAX_FILE_NAME]).is_some());
    assert_eq!(
        bytes_cost(-1),
        u32::MAX as u64 / WORK_BYTES,
        "lengths are unsigned"
    );
}
