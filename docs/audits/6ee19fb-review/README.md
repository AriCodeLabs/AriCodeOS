# Evidencias de la revisión de 6ee19fb

Commit: `6ee19fb2b5d64887d5d9ccba3215641727ef7fe9`. Fecha: 2026-09-29.
Informe: [supervisión, persistencia, paquetes y WASM](../2026-09-29-services-persistence-packages-wasm-6ee19fb.md).

## Archivos

| Archivo | Contenido |
| --- | --- |
| `check.log` | `cargo xtask check` sobre la copia del commit. |
| `tests.log` | Batería oficial completa; contiene también resultados de host. |
| `host/src/lib.rs`, `host/Cargo.toml` | Siete sondas adicionales sobre las bibliotecas originales. |
| `host-probes.log` | Resultado de las siete aserciones; fallan al exigir el comportamiento correcto. |
| `reference.log`, `*.wasm` | Comparación con `WebAssembly.validate` y ejecución del loop en Node. |
| `app/` | Aplicación de prueba que se empaqueta dos veces, como `reviewa` y `reviewb`, con `caps = fs`. |
| `app-build.log`, `prepare.log`, `prepare.rs` | Construcción de la app y preparación de paquetes/disco. |
| `journal-wasm-serial.log`, `journal-wasm-command.json` | Sesión QEMU: restos del journal, importación con firma incompatible y transacción abandonada. |
| `supervisor-timeout-serial.log`, `supervisor-timeout-command.json` | Sesión QEMU: servicio que sigue vivo tras vencer su sonda. |
| `run-qemu.py`, `prepare-supervisor.py`, `run-supervisor.py` | Harness y preparación de fixtures utilizados, con las rutas concretas de esta revisión. |

No se guardan imágenes de disco, binarios ELF ni directorios de compilación en
el repositorio. Los módulos `.wasm` son fixtures mínimos, con datos sintéticos.
Las sondas no modifican fuentes de producción.

## Batería oficial aislada

Se exportó el commit mediante `git archive` a
`/var/tmp/aricode-review-6ee19fb-uhBqbF`. En archivos `.rs`, `.toml` y `.md`
de `sdk`, `tests`, `tools`, `apps` y `user` se sustituyeron los números
17007, 17008, 17009 y 17010 por 31007, 31008, 31009 y 31010, respectivamente.
Esto incluye la URL HTTP y los textos esperados de red.

Desde esa copia:

```bash
cargo xtask check > review-check.log 2>&1
cargo xtask test --all > review-tests.log 2>&1
```

La suite usa las CPUs declaradas por cada escenario; no se forzó toda la
batería a cuatro CPU. Las pruebas concurrentes del árbol original no se
interrumpieron y sus resultados no se mezclan con los de esta auditoría.

## Sondas de host

Desde una copia del repositorio en el commit revisado que incluya esta carpeta
de evidencias:

```bash
CARGO_TARGET_DIR=/var/tmp/aricode-review-host-target cargo test \
  --manifest-path docs/audits/6ee19fb-review/host/Cargo.toml -- --nocapture
```

Resultado esperado en `6ee19fb`: siete tests fallidos (salida de Cargo 101).
Se comprueba la conducta deseada, no se escribe un test que considere el defecto
un éxito. Al ejecutar, Cargo puede crear `Cargo.lock` y los cuatro fixtures WASM
en el directorio del proyecto de las sondas.

El disco simulado distingue sectores pendientes y persistidos. `flush` hace
duraderas todas las escrituras anteriores. Un corte conserva solo los sectores
de tabla pendientes y descarta los demás; conserva sectores completos, nunca
sectores rotos. Es una ejecución permitida por el contrato de `BlockDevice` que
no garantiza orden antes de `flush`.

Las sondas originales se ejecutaron en `/var/tmp/aricode-probes-6ee19fb`, con
dependencias por ruta a la copia de `6ee19fb`. Los manifiestos archivados usan
rutas relativas al repositorio para facilitar su repetición.

Comparación independiente, desde la raíz del repositorio:

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

Los tres primeros deben rechazarse; el loop devuelve 0. `wrong-import.wasm`
es estructuralmente válido: su incompatibilidad se produce al enlazarlo contra
la firma de `aricode.file_write`, no al validar su estructura independiente.

## Reproducciones QEMU

Los scripts conservan las rutas originales. Para otro directorio, sustituir
`/var/tmp/aricode-review-6ee19fb-uhBqbF` y `/var/tmp/aricode-probes-6ee19fb`
en scripts, manifiestos temporales y herramienta de preparación. Usar una copia
aislada: `prepare.rs` modifica el disco que recibe y `prepare-supervisor.py`
modifica el initrd y slots de su propia copia de prueba.

Preparación de la primera sesión:

1. Construir el commit y dejar que termine `boot.kernel_ready`. La revisión
   tomó su `build/scenarios/boot.kernel_ready` como disco/ESP de partida.
2. Compilar `app/` para `x86_64-unknown-none`, con el linker script del SDK:
   `RUSTFLAGS='-C relocation-model=static -C code-model=kernel' cargo build
   --manifest-path app/Cargo.toml --target x86_64-unknown-none --release`.
   Usar `strip --strip-debug` para dejar `review_app.elf` en el directorio de sondas.
3. Copiar el escenario a `qemu/`, escribir `qemu_exit shell` en
   `qemu/esp/aricodeos/cmdline.txt` y restaurar `OVMF_VARS.fd` desde
   `/usr/share/OVMF/OVMF_VARS_4M.fd`.
4. Compilar `prepare.rs` como binario de host con dependencias `aricode-arifs`,
   `aricode-wasm`, `aricode-crypto` y `aricode-pkg` (feature `write`) por ruta
   al commit. Ejecutarlo desde el directorio de sondas con `qemu/disk.img`.
   Usa exclusivamente la clave de desarrollo del repositorio. Instala dos
   paquetes de la app de prueba y sustituye `wasm_runner/demo.wasm` por el
   módulo mínimo de importación incompatible.
5. Ejecutar `run-qemu.py`. En esta revisión seguía viva la batería oficial,
   cuyos respondedores de pruebas 31007/31008/31010 también atendieron las
   autopruebas de red del invitado adicional. Fuera de ese contexto deben
   mantenerse esos respondedores; de lo contrario init puede fallar antes
   de llegar al shell. El invitado adicional no abre `hostfwd`.

La sesión envía, esperando cada prompt:

```text
run reviewa 1
run reviewb 2
run wasm_runner
run reviewa 3
run reviewb 4
shutdown
```

Los dos primeros comandos reproducen el cruce de datos del journal. El tercero
produce el panic del runner. Los dos siguientes prueban el journal ocupado
después de morir el dueño de una transacción. QEMU termina con 33; ese código
solo confirma el apagado normal del sistema, no el éxito de las propiedades
que se están comprobando.

Para el supervisor, ejecutar `prepare-supervisor.py` y después
`run-supervisor.py`, con los mismos respondedores activos. El fixture cambia
solo `virtio_input` por el binario `cpu_hog` ya presente en el initrd. Invalida
el estado A/B y la cabecera del slot A de su disco para usar la recuperación
desde la ESP modificada. El kernel y el código del supervisor no se modifican.
`services --detail`, `ps` y `shutdown` muestran el desacuerdo entre el estado
del supervisor y los procesos vivos que conservan los dispositivos PCI.
