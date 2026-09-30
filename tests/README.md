# Tests de AriCodeOS

Todos los tests viven en el repositorio y se ejecutan con `cargo xtask test`.

## Niveles

| Nivel   | Qué ejecuta                                              | Duración | Cuándo es obligatorio                    |
|---------|----------------------------------------------------------|----------|------------------------------------------|
| `unit`  | `cargo test` de las crates de host (`libs/*`, protocolo, xtask) | segundos | en cada commit de una feature            |
| `smoke` | unit + arranque nominal en QEMU (`boot.kernel_ready`)     | ~5 s     | antes de mergear en `develop`            |
| `full`  | unit + todos los escenarios QEMU (fallos provocados incluidos) | ~20 s    | antes de mergear en `main` (`--all`)     |

```bash
cargo xtask test                    # unit + smoke (por defecto)
cargo xtask test --level unit       # solo host
cargo xtask test --all              # todo: obligatorio hacia main
cargo xtask test --area boot        # escenarios QEMU del área (con --all para incluir los full)
cargo xtask test --list             # ver escenarios y cuáles selecciona el filtro
```

## Dónde está cada cosa

```text
libs/*/src/lib.rs        #[cfg(test)] unitarios de ELF, paginación, cmdline
boot-protocol/src/lib.rs #[cfg(test)] clasificación de memoria, iterador, cmdline
tools/xtask/src/*.rs     #[cfg(test)] validez de los escenarios, utilidades
tests/qemu/*.toml        escenarios de integración en QEMU
```

## Escribir un escenario QEMU

Un fichero `tests/qemu/<area>_<nombre>.toml`:

```toml
name = "area.nombre"            # único; solo [a-z0-9._-]; también es el directorio en build/scenarios/
description = "una línea"
level = "smoke" | "full"
areas = ["boot", "memory"]      # para filtrar con --area
cmdline = "qemu_exit ..."       # se escribe en aricodeos/cmdline.txt
timeout_secs = 30
stdin = ["ls", "shutdown"]      # opcional: líneas que el runner teclea por la serie
prompt = "ari> "                # cada línea se envía al ver el prompt una vez más (por defecto "ari> ")

[host_tcp]                      # opcional: sonda TCP del host hacia el invitado (hostfwd 127.0.0.1:17009 -> :7)
marker = "TCP_LISTEN_READY"     # al verlo en la serie, xtask se conecta y envía payload
payload = "hola desde el host"  # el eco se anota en la captura como "[host] sonda TCP: ..."

disk = "slots"                  # disco: slots (defecto) | esp-only | pending-b | b-exhausted | b-corrupt | usb-image (la ESP va dentro del disco)
                                #        | fs-pending-replace (reemplazo de AriFS comprometido sin aplicar) | fs-corrupt (tabla de AriFS alterada)
                                #        | wasm-restricted (manifiesto del modulo solo con print) | wasm-invalid (demo.wasm mal tipado)
                                #        | wasm-bad-import (demo.wasm importa una funcion del anfitrion con otra firma)
disk_readonly = false           # true: el disco de pruebas se presenta como solo lectura
cpus = 4                        # procesadores (-smp N); sin declarar: 1, o lo que fuerce `--cpus N`
usb_hid = true                  # añade qemu-xhci con un teclado y un ratón USB (los eventos QMP van al USB)
usb_legacy_controller = true    # añade además un EHCI (clase 0c.03 pero no xHCI)
install = ["examples/hello_ext"]  # apps externas que xtask compila, firma (version 1) e instala en AriFS como apps/NOMBRE.aripkg
packages = ["examples/hello_ext:2", "examples/hello_ext:3:corrupt"]  # paquetes pendientes en pkgs/ (DIR:VERSION[:corrupt]) para `run installer`

[host_input]                    # opcional: entrada inyectada por QMP al ver marker
marker = "INPUT_READY"
keys = ["h", "o", "l", "a", "ret"]   # nombres qcode de QEMU, pulsación y suelta en kbd0
mouse_rel = [10, -5]            # movimiento relativo en mouse0
click = "left"                  # botón pulsado y soltado; resultado en "[host] QMP: ..."

[inject]                        # opcional: fallos provocados en la ESP
truncate_kernel = 3000          # recorta kernel.elf a N bytes
remove_kernel = true            # elimina kernel.elf
patch_entry = 0                 # sobrescribe e_entry del ELF
add_empty_segment_at_entry = true   # añade un PT_LOAD RW vacío (memsz 0) en e_entry

[expect]
exit = 33                       # 33 = éxito, 35 = fallo reportado por el sistema, 0 = reboot (-no-reboot)
serial_contains = ["..."]
serial_absent = ["..."]
```

Un escenario con varios arranques sobre el mismo disco declara `[[boots]]` en
lugar de `cmdline`, `stdin` y `[expect]` (el disco de pruebas se fabrica una
vez y se conserva; cada arranque tiene su ESP y su `serial-N.log`):

```toml
[[boots]]
cmdline = "qemu_exit shell"
stdin = ["run updater", "reboot"]
kill_on = "updater: imagen en UPDATE version 2 verificada ("   # opcional: corte de energía (mata QEMU al ver el texto)
[boots.expect]
exit = 0                        # o `killed = true` cuando se espera el corte
serial_contains = ["..."]

[[boots]]
cmdline = "qemu_exit"
[boots.expect]
exit = 33
serial_contains = ["AriInit: arranque confirmado en el slot B"]
```

Los fallos provocados dentro del kernel se piden por cmdline (`test=panic`),
nunca con features de compilación: el mismo binario sirve para todos los
escenarios y no queda código de test disperso por `cfg`. Los programas de
ring 3 (`test=user_<nombre>`) viven en `user/testprogs` como binarios propios
(Rust sobre `aricode-sys` o ensamblador) y devuelven un código de salida que
identifica el paso fallido.

## Áreas

Cada fase del roadmap añade su área y sus escenarios. Existentes: `boot`,
`panic`, `console`, `interrupts`, `memory`, `timer`, `threads`, `scheduler`,
`user`, `capabilities`, `ipc`, `init`, `drivers`, `acpi`, `pci`, `virtio`,
`storage`, `filesystem`, `shell`, `network`, `input`, `gui`, `sandbox`, `wasm`, `crypto`, `update`, `smp`, `iommu`, `usb`, `hardware`, `services`.
Una rama `feature/f05-physical-memory` ejecuta `--area memory` mientras
desarrolla y `--all` antes de integrar. Si el filtro no selecciona ningún
escenario QEMU, el runner falla y lista las áreas conocidas: un filtro mal
escrito nunca produce un éxito vacío.

## Varias CPUs

Los escenarios arrancan con una CPU salvo que declaren `cpus = N`; los de
la Fase 32 usan 4 (y `cpus = 1` el que prueba la degradación a una sola).
`cargo xtask test --all --cpus 4` arranca con cuatro procesadores todos los
escenarios que no declaran `cpus`: es la prueba de estrés del kernel SMP y debe
pasar entera antes de integrar cambios en el planificador, los locks o las
rutas de entrada a ring 0. Las autopruebas con expectativas de reparto en una
sola CPU (`scheduler.selftest`, `threads.selftest`) fijan sus hilos a la
CPU 0 para valer con cualquier `--cpus`.

## Logs

Cada escenario deja su serie en `build/scenarios/<name>/serial.log`. El runner
se niega a trabajar si ese directorio o su `esp` son enlaces simbólicos o si la
ruta canónica sale de `build/scenarios/`.
