# SDK de aplicaciones (Fase 27)

Un programa de AriCodeOS es un ELF estático de ring 3 cargado en `0x400000`
que solo puede hacer lo que sus handles le permiten. El SDK son cuatro crates
de `sdk/`:

| Crate | Qué da |
|-------|--------|
| `aricode-abi` (`sdk/abi`) | números de syscall, errores, tipos de objeto, derechos, handles iniciales |
| `aricode-sys` (`sdk/rust`) | wrappers de syscall y clientes de servicios: `fs`, `net`, `http`, `input`, `window`, `block`, `time`, `permissions`, `virtio_pci` |
| `aricode-proto` (`sdk/proto`) | los protocolos IPC versionados de cada servicio |
| `aricode-rt` (`sdk/runtime`) | `entry!`, heap de 256 KiB, panic handler y utilidades de formato; reexporta `aricode-sys` como `sys` |
| `aricode-build` (`sdk/build`) | `build.rs` de una línea que enlaza con `sdk/rust/user.ld` |

## Programa mínimo

`Cargo.toml`:

```toml
[package]
name = "mi-app"
version.workspace = true
edition.workspace = true

[[bin]]
name = "mi_app"
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
    sys::print(b"hola\n");
    0 // codigo de salida
}

aricode_rt::entry!(main);
```

Para que llegue al initrd, el paquete se añade a `Cargo.toml` (workspace), a
`PACKAGES` y a `build_user_programs`/`initrd_programs` en
`tools/xtask/src/build.rs`. `apps/hello` es el ejemplo completo.

## Qué recibe un programa

Handles fijos (`aricode_abi::handle`): `0x10001` su proceso, `0x10002` su
hilo principal, `0x10003` la consola (`WRITE`). Todo lo demás lo entrega
quien lo lanza, en orden, a partir de `0x10004`, y el `arg` de `main` es el
valor que ese lanzador eligió. No hay variables de entorno ni descubrimiento
de servicios: un programa que no recibe el endpoint de un servicio no puede
hablar con él. `sys::permissions::granted` recorre lo recibido.

## APIs

| Área | Cómo |
|------|------|
| process | `sys::process_spawn_with(spawner, nombre, arg, handles)`, `sys::process_wait`, `sys::process_exit`; hace falta un handle `SPAWNER` con `MANAGE` (init lo tiene; el shell recibe uno para `run`) |
| ipc | `sys::endpoint_create`, `sys::ipc_send`, `sys::ipc_receive` (256 bytes y hasta 4 handles por mensaje, con plazo); los datos grandes van por buffers DMA compartidos |
| filesystem | `sys::fs::FsClient` sobre el endpoint de `fsd` y una capacidad `HARDWARE` para su buffer: `list`, `open`, `create`, `read`, `write` (en el sitio, duradero tras `flush`), `replace` (todo-o-nada y duradero, hasta 4 KiB), `flush`, `close` (ADR 0014) |
| network | `sys::net::NetClient`: `info`, `ping`, UDP (`udp_bind/send/recv`), TCP (`tcp_connect/listen/accept/send/recv/close`), `resolve`; `sys::http::get` encima |
| window | `sys::window::Window::create(compositor, hardware, w, h, titulo)`, `canvas()` (lienzo `aricode-gfx`), `present`, `poll` (`Key`, `Click`, `Focus`), `close` |
| input | `sys::input::InputClient` sobre un `inputd`: eventos evdev con instante; `aricode-input::Keyboard` los traduce |
| console | `sys::console_write` (`WRITE`), `sys::console_read` (`READ`, con plazo); `sys::console_inject` exige `MANAGE` y solo lo usa el puente de consola de init (ADR 0013) |
| time | `sys::time::uptime_ms(system)`, `sys::time::Clock`, `sys::time::sleep_ms` |
| permissions | `sys::permissions::{granted, kind_name, is, drop_handle}` |

## Manifiesto y lanzamiento (Fase 28)

Una aplicación se lanza a través del broker (init) con `run NOMBRE [ARG]`
desde el shell o `sys::launch::run(broker, nombre, arg)`; `spawn` y
`sys::launch::spawn` la lanzan en segundo plano sin esperar su salida. El
broker admite 4 programas en marcha a la vez y responde `Limit` al quinto
(ADR 0015). El broker lee
`NOMBRE.manifest` del initrd (`apps/<app>/NOMBRE.manifest`, empaquetado por
xtask):

```text
name = hello_app
caps = system, fs      # system | fs | net | window | input | block (solo apps de confianza: updater)
memory = 128K          # cuota de buffers DMA (K o M, máximo 8M)
```

Los handles llegan en este orden fijo, solo los pedidos: `SYSTEM` (lectura),
directorio privado de fsd, endpoint de red, endpoint del compositor,
endpoints de entrada y, si alguna capacidad necesita buffers (`fs`, `net`,
`window`), `HARDWARE` al final. Con `sys::permissions::granted` la aplicación
los reconoce por tipo sin depender de la posición. Sin manifiesto solo se
recibe consola y `SYSTEM`. Pedir `spawner`, `pci` o `framebuffer` hace que el
broker deniegue el lanzamiento.

## Reglas

- Para entregar un handle a otro proceso hace falta `TRANSFER` sobre él, y
  para hacer una copia con menos derechos, `DUPLICATE`. Quien lanza decide
  qué derechos viajan; nunca se pueden ampliar (`Error::Escalation`).

- Sin `alloc` en las bibliotecas del SDK; el programa tiene un heap estático
  de 256 KiB si lo necesita (`extern crate alloc`).
- Todo error del kernel llega como `sys::Error`; los servicios responden con
  su `Status` propio (`aricode-proto`). Nunca se recortan nombres: si no
  caben, la llamada falla.
- Un programa que hace algo ilegal (tocar memoria del kernel, instrucciones
  privilegiadas, pila desbordada) muere solo; el kernel y el resto siguen.

## Plazos y correlación de las llamadas (ADR 0012)

Todas las llamadas de los clientes del SDK (`block`, `fs`, `net`, `window`,
`input`, `launch`, `status`) pasan por `aricode_sys::rpc::call`:

- cada petición crea su propio endpoint de respuesta y lo cierra al volver;
  una respuesta tardía se pierde y nunca se atribuye a otra petición;
- el envío y la recepción tienen plazo (el pedido por la petición más
  `rpc::GRACE_MS`); un servicio muerto produce `Error::Timeout`, no una
  espera infinita. Solo `launch::run` espera sin plazo la salida del
  programa lanzado;
- un plazo vencido no deshace la operación: su resultado es desconocido. El
  SDK no reintenta escrituras; el programa decide. Las lecturas y consultas
  sin efectos se pueden repetir.
