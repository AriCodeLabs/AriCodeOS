# Perfil `aricode-wasm` v1

Lo que un módulo WebAssembly puede contener y usar en AriCodeOS. Es un
perfil propio y explícito (ADR 0019); **no es WASI** ni el Component Model:
un módulo escrito para este perfil no asume nada de POSIX, y un módulo WASI
no funciona aquí. Todo lo que no aparece en esta página no existe para el
módulo.

## Formato binario

- Versión 1 del formato binario (`\0asm` + 1). Secciones: type, import,
  function, table, memory, global, export, start, element, code, data y
  custom (ignoradas).
- Tipos de valor: `i32` e `i64`. `f32` y `f64` se rechazan (constantes,
  operaciones, cargas, globales y conversiones): `Unsupported("coma
  flotante")`.
- Extensiones aceptadas sobre el MVP: `sign-ext` (`i32.extend8_s`,
  `i32.extend16_s`, `i64.extend8_s`, `i64.extend16_s`, `i64.extend32_s`),
  `bulk-memory` solo `memory.copy` y `memory.fill`, `select` con tipo
  (`select t`), tipos de bloque por índice de tipo (parámetros y varios
  resultados en bloques).
- No hay SIMD, hilos, referencias (`ref.*`, tablas de `externref`),
  excepciones, memorias múltiples, `memory.init`/`data.drop` ni
  `table.*`.
- Una tabla `funcref` como máximo (hasta 4096 entradas), segmentos de
  elementos activos con desplazamiento `i32.const`.
- Una memoria como máximo; sus páginas iniciales deben caber en la que da
  el anfitrión (4 páginas, 256 KiB). `memory.grow` crece dentro de ese
  buffer y devuelve -1 si no cabe.
- Importaciones: solo funciones, y solo del módulo `aricode`.
- Límites: 4096 locales por función, 1024 bloques anidados, módulo de
  hasta 64 KiB (lo carga el runner), 64 llamadas anidadas, 16 Ki valores en
  la pila y 50 M pasos por llamada exportada.

## Validación

`Module::parse` valida el módulo entero antes de devolverlo: índices de
tipos, funciones, tabla, globales, exportaciones y `start`, y cada cuerpo
con el algoritmo de tipado de la especificación (pila de operandos tipada,
marcos de control, código inalcanzable polimórfico). Un módulo que no
valida no se instancia: `Error::Invalid(motivo)`.

Para comprobar un módulo con herramientas externas antes de llevarlo al
sistema:

```bash
wasm-tools validate --features=mvp,sign-ext,bulk-memory demo.wasm
```

(`wasm-tools` no forma parte de las herramientas del repositorio; la
comprobación de referencia es la del propio validador, `cargo test -p
aricode-wasm`.)

## Cómo se produce un módulo

`rustc` para `wasm32-unknown-unknown` con `-C target-cpu=mvp -C
target-feature=+sign-ext,+bulk-memory` y sin coma flotante en el código
(`.cargo/config.toml`, `apps/wasm-demo`). Exportar `main(i64) -> i32`; el
resultado es el código de salida del runner.

## Manifiesto del módulo

Junto al módulo va `NOMBRE.manifest` (texto, `#` comenta):

```text
caps = print, clock, files
```

| Capacidad | Funciones que habilita |
|---|---|
| `print` | `print` |
| `clock` | `uptime_ms` |
| `files` | `file_read`, `file_write` |

Una importación cuya capacidad no está concedida hace que el runner no
ejecute el módulo (`importacion aricode.X no concedida por el manifiesto
del modulo`, código 12). Sin manifiesto no se ejecuta (código 11).

## Capability API (`aricode`)

| Función | Firma | Semántica |
|---|---|---|
| `print` | `(ptr: i32, len: i32)` | Escribe `len` bytes de la memoria del módulo en la consola. Rango fuera de memoria: trap. |
| `uptime_ms` | `() -> i64` | Milisegundos desde el arranque (0 si el runner no tiene `SYSTEM`). |
| `file_write` | `(name: i32, name_len: i32, data: i32, len: i32) -> i32` | Crea o reemplaza entero (atómico y duradero, ADR 0014) un fichero del directorio privado del runner. Devuelve los bytes escritos o -1. Nombre de 1 a 32 bytes UTF-8 sin `/`; si no, trap. |
| `file_read` | `(name: i32, name_len: i32, buf: i32, len: i32) -> i32` | Lee el fichero en `buf` (hasta `len`). Devuelve los bytes leídos o -1 si no existe. |

Las funciones que hablan con un servicio (`file_*`) heredan los plazos del
SDK (ADR 0012): un servicio muerto produce -1, nunca una espera infinita.
El contador de pasos del intérprete no cuenta el tiempo dentro de una
importación.

## Errores del runner

| Código | Motivo |
|---|---|
| 3 | no hay `demo.wasm` |
| 5 | módulo de más de 64 KiB |
| 6 | módulo malformado, no soportado o inválido (se imprime el motivo) |
| 7 | importación desconocida |
| 11 | manifiesto ausente o inválido |
| 12 | importación no concedida por el manifiesto |
| 13 | trap, pasos agotados o pila agotada en ejecución |
