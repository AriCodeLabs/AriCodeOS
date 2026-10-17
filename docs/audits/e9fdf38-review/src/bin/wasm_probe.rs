// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
use aricode_wasm::{Error, Host, Instance, Module, Value, PAGE};
struct NoHost;
impl Host for NoHost {
    fn call(&mut self, _: usize, _: &[Value], _: &mut [u8]) -> Result<Value, Error> {
        Err(Error::MissingImport)
    }
}
fn leb(mut n: usize) -> Vec<u8> {
    let mut b = Vec::new();
    loop {
        let byte = (n & 127) as u8;
        n >>= 7;
        b.push(byte | if n > 0 { 128 } else { 0 });
        if n == 0 {
            return b;
        }
    }
}
fn section(id: u8, bytes: &[u8], out: &mut Vec<u8>) {
    out.push(id);
    out.extend(leb(bytes.len()));
    out.extend(bytes);
}
fn module(code: &[u8]) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    section(1, &[1, 0x60, 0, 1, 0x7f], &mut out);
    section(3, &[1, 0], &mut out);
    section(5, &[1, 1, 1, 1], &mut out); // min=max=1 page
    section(7, &[1, 1, b'f', 0, 0], &mut out);
    let mut body = vec![0];
    body.extend(code);
    body.push(0x0b);
    let mut sec = vec![1];
    sec.extend(leb(body.len()));
    sec.extend(body);
    section(10, &sec, &mut out);
    out
}
fn main() {}
#[test]
fn load_past_declared_memory_must_trap() {
    let wasm = module(&[0x41, 0x80, 0x80, 0x04, 0x2d, 0, 0]); // i32.const 65536; i32.load8_u
    let m = Module::parse(&wasm).unwrap();
    let mut memory = vec![0; 4 * PAGE];
    memory[PAGE] = 0x5a;
    let mut i = Instance::new(&m, &mut memory).unwrap();
    let result = i.call("f", &[], &mut memory, &mut NoHost);
    println!("read at 65536 with max=1 page: {result:?}");
    assert!(matches!(result, Err(Error::Trap(_))));
}
#[test]
fn instantiation_must_zero_memory() {
    let wasm = module(&[0x41, 0, 0x2d, 0, 0]);
    let m = Module::parse(&wasm).unwrap();
    let mut memory = vec![0xaa; PAGE];
    let mut i = Instance::new(&m, &mut memory).unwrap();
    assert_eq!(i.call("f", &[], &mut memory, &mut NoHost), Ok(0));
}
#[test]
fn start_function_must_run_or_be_rejected() {
    let mut wasm = b"\0asm\x01\0\0\0".to_vec();
    section(1, &[2, 0x60, 0, 0, 0x60, 0, 1, 0x7f], &mut wasm);
    section(3, &[2, 0, 1], &mut wasm);
    section(6, &[1, 0x7f, 1, 0x41, 0, 0x0b], &mut wasm);
    section(7, &[1, 1, b'f', 0, 1], &mut wasm);
    section(8, &[0], &mut wasm);
    section(
        10,
        &[2, 6, 0, 0x41, 42, 0x24, 0, 0x0b, 4, 0, 0x23, 0, 0x0b],
        &mut wasm,
    );
    let m = Module::parse(&wasm).unwrap();
    let mut mem = [];
    let mut i = Instance::new(&m, &mut mem).unwrap();
    assert_eq!(i.call("f", &[], &mut mem, &mut NoHost), Ok(42));
}

#[test]
fn grown_memory_must_be_zero() {
    let mut wasm = module(&[0x41, 1, 0x40, 0, 0x1a, 0x41, 0x80, 0x80, 0x04, 0x2d, 0, 0]);
    let at = wasm
        .windows(6)
        .position(|s| s == [5, 4, 1, 1, 1, 1])
        .unwrap();
    wasm[at + 5] = 2;
    let m = Module::parse(&wasm).unwrap();
    let mut memory = vec![0xaa; 2 * PAGE];
    let mut i = Instance::new(&m, &mut memory).unwrap();
    assert_eq!(i.call("f", &[], &mut memory, &mut NoHost), Ok(0));
}
