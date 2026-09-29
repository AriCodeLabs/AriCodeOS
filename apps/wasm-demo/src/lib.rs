//! Aplicación portable (Fase 29 y etapa 4): se compila a
//! `wasm32-unknown-unknown` y solo conoce la Capability API que el anfitrión
//! (`wasm_runner`) le importa bajo el módulo `aricode`, y solo lo que su
//! manifiesto (`demo.manifest`) le concede: nada de POSIX ni de syscalls.
#![no_std]

#[link(wasm_import_module = "aricode")]
extern "C" {
    /// Escribe `len` bytes de la memoria del módulo en la consola.
    fn print(ptr: *const u8, len: u32);
    /// Milisegundos desde el arranque del sistema.
    fn uptime_ms() -> i64;
    /// Crea o reemplaza entero un fichero del directorio privado del runner;
    /// devuelve los bytes escritos o -1.
    fn file_write(name: *const u8, name_len: u32, data: *const u8, len: u32) -> i32;
    /// Lee un fichero del directorio privado en `buf`; bytes leídos o -1.
    fn file_read(name: *const u8, name_len: u32, buf: *mut u8, len: u32) -> i32;
}

fn say(s: &[u8]) {
    // SAFETY: `s` está en la memoria lineal del módulo; el anfitrión valida el rango.
    unsafe { print(s.as_ptr(), s.len() as u32) };
}

fn fmt_u64(mut v: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut i = buf.len();
    if v == 0 {
        i -= 1;
        buf[i] = b'0';
    }
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    &buf[i..]
}

fn fib(n: u32) -> u64 {
    let (mut a, mut b) = (0u64, 1u64);
    for _ in 0..n {
        let t = a + b;
        a = b;
        b = t;
    }
    a
}

/// Punto de entrada que invoca el anfitrión: `arg` es el argumento del
/// lanzamiento; el resultado es el código de salida.
#[no_mangle]
pub extern "C" fn main(arg: i64) -> i32 {
    let mut buf = [0u8; 20];
    say(b"wasm: hola desde WebAssembly (arg ");
    say(fmt_u64(arg as u64, &mut buf));
    say(b")\n");
    say(b"wasm: fib(24) = ");
    say(fmt_u64(fib(24), &mut buf));
    say(b"\n");
    // SAFETY: importación sin argumentos.
    let ms = unsafe { uptime_ms() };
    say(b"wasm: uptime ");
    say(fmt_u64(ms as u64 / 1000, &mut buf));
    say(b" s\n");
    // Una tabla en memoria: los datos del modulo llegan a la memoria lineal.
    let table = [3u32, 1, 4, 1, 5, 9, 2, 6];
    let mut sum = 0u32;
    for (i, v) in table.iter().enumerate() {
        sum = sum.wrapping_add(v * (i as u32 + 1));
    }
    say(b"wasm: suma ponderada ");
    say(fmt_u64(sum as u64, &mut buf));
    say(b"\n");
    // Un fichero del directorio privado del runner: escribir y releer.
    let name = b"notas";
    let text = b"escrito desde wasm";
    let mut back = [0u8; 32];
    // SAFETY: punteros a la memoria lineal del módulo; el anfitrión valida.
    let (written, read) = unsafe {
        (
            file_write(
                name.as_ptr(),
                name.len() as u32,
                text.as_ptr(),
                text.len() as u32,
            ),
            file_read(
                name.as_ptr(),
                name.len() as u32,
                back.as_mut_ptr(),
                back.len() as u32,
            ),
        )
    };
    if written as usize != text.len() || read as usize != text.len() || &back[..text.len()] != text
    {
        say(b"wasm: el fichero no volvio igual\n");
        return 3;
    }
    say(b"wasm: fichero 'notas' escrito y leido (");
    say(fmt_u64(text.len() as u64, &mut buf));
    say(b" bytes)\n");
    42
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}
