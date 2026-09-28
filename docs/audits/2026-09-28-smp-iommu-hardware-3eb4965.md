# Revisión de SMP, IOMMU y preparación para hardware real

Fecha: 2026-09-28. Commit revisado: `3eb4965ab6eed03cd4261975a6b6f251d3541c7a`.
Comparación: `ade2d04..3eb4965` (92 archivos). Incluye las correcciones de la
auditoría anterior, Fase 32 (SMP), Fase 33 (VT-d) y preparación de Fase 34
(imagen USB, FAT32, PS/2 y opciones de diagnóstico).

Dos hallazgos confirmados: un P1 y un P2. No se han aplicado correcciones al
código de producción. Las líneas citadas corresponden al commit revisado.

## Validación

| Comprobación | Resultado |
| --- | --- |
| `cargo xtask check` | Correcto |
| `cargo xtask test --all` | Correcto: 203 pruebas de host y 77 escenarios QEMU, 0 fallidos |
| Recogida concurrente de un proceso, QEMU con 4 CPU | Confirmada retirada del proceso con su mapper aún presente |
| Reasignación del contexto VT-d, QEMU | Confirmada invalidación del contexto del segundo dominio al liberar el primero |

Las pruebas se ejecutaron sobre copias de `git archive 3eb4965`, con puertos
de red distintos de los del árbol de trabajo. La batería oficial usa su
configuración de CPU por escenario; no equivale a repetir todos los casos
con cuatro CPU. Las dos pruebas adicionales instrumentan exclusivamente
otra copia temporal. No se ha probado un portátil físico.

Evidencias y parches de instrumentación: [3eb4965-review](3eb4965-review/README.md).

## 1. [P1] Esperar un proceso puede retirar su mapper antes de liberar sus páginas

Código: [kernel/src/task/scheduler.rs](../../kernel/src/task/scheduler.rs),
líneas 389–404; [kernel/src/user/process.rs](../../kernel/src/user/process.rs),
líneas 319–344 y 348–362.

Con SMP, `reap` elimina el hilo de la política y de la tabla bajo `SCHED`,
pero libera el espacio de direcciones después de soltar ese lock. En esa
ventana, otra CPU puede ejecutar `finished`: considera terminado y recogido
el proceso porque `scheduler::state(main_thread)` ya devuelve `None`.
`wait_until` elimina entonces su entrada de `PROCESSES`.

Cuando la primera CPU llega a `release_address_space`, ya no encuentra el
proceso ni puede recuperar su mapper. Soltar el mapper no devuelve sus frames
al PMM: eso requiere `free_user_half` y `into_root_frame`, precisamente las
operaciones que se han omitido. Se pierden las páginas de usuario y sus
tablas. La repetición de creación y espera de procesos puede agotar la RAM.
La exclusión de interrupciones local que protegía la secuencia en una CPU
no impide esta intercalación entre CPU distintas.

Reproducción: el escenario `user.hello` con cuatro CPU y una barrera de prueba
al entrar en `release_address_space`. La barrera deja ejecutar la retirada
desde `wait_until` y después permite continuar la recogida. Resultado:

```text
AUDIT wait: removed process with mapper=true
AUDIT reaper: process_present=false
```

El escenario termina correctamente con `Exited(42)`: su éxito funcional no
detecta la pérdida de páginas. Esta prueba fuerza una intercalación permitida;
no mide la frecuencia de aparición sin instrumentación ni pretende ser una
prueba de agotamiento de memoria.

Corrección propuesta: publicar un estado de recogida completada después de
liberar el espacio de direcciones y el dominio DMA, y hacer que `wait_until`
espere ese estado antes de retirar el proceso. Mantener el orden de locks
`PROCESSES -> SCHED`; no resolverlo tomando `PROCESSES` dentro de `SCHED`.

Regresión: una prueba con barrera que mantenga el proceso visible mientras
la recogida está pendiente, seguida de creación/espera repetida en varias
CPU y comprobación de recuperación de frames.

## 2. [P2] Liberar el antiguo dominio desconecta un dispositivo ya reasignado

Código: [kernel/src/iommu.rs](../../kernel/src/iommu.rs), líneas 482–490 y
534–547. Ruta de entrada: `mmio_map`, en
[kernel/src/user/syscalls.rs](../../kernel/src/user/syscalls.rs), líneas 443–470.

`attach` escribe el nuevo contexto del dispositivo y añade `(unit, source)`
al dominio receptor, pero conserva esa misma anotación en el dominio
anterior. Cuando termina el proceso anterior, `release` pone a cero todos
los contextos de su lista sin comprobar quién es su propietario actual.
El dispositivo del segundo proceso pierde entonces su traducción DMA.

La secuencia es alcanzable mediante las capabilities MMIO de PCI: se crean
con `TRANSFER`, pueden pasar a otro proceso y cada `mmio_map` llama a
`attach` con el PID receptor. No hace falta reclamar por segunda vez el
dispositivo con `pci::claim`.

Reproducción adicional dentro de `iommu::selftest`: asignar la función
`00:02.0` al dominio del PID de prueba 1001, reasignarla al del 1002 y liberar
el primero. Se consultan las entradas reales de contexto construidas por el
kernel en QEMU:

```text
AUDIT context before release: present=true domain=3
AUDIT context after release: present=false domain=0
```

La aserción de que el contexto del segundo dominio sigue presente falla.
La prueba llama directamente a la implementación IOMMU con identificadores
de prueba; no ejecuta una transferencia completa entre dos drivers ni mide
una operación de E/S posterior. La pérdida del contexto queda comprobada.

Corrección propuesta: definir y mantener un propietario único por función
PCI. Si se permite la reasignación, retirar la asociación anterior y validar
la propiedad antes de borrar un contexto en `release`. Si no se permite,
rechazar el segundo `attach` antes de modificar las tablas. La lista del
dominio tampoco debe marcar la operación como completada antes de que
`context_entry` y la escritura del contexto hayan tenido éxito.

Regresión: asignar a A, transferir a B, terminar A y verificar que B conserva
el contexto y puede completar E/S; liberar B debe retirar finalmente el
contexto.

## Correcciones anteriores y límites

Se han revisado las ocho correcciones integradas de la
[auditoría anterior](2026-09-27-sandbox-wasm-crypto-ab-e9fdf38.md): propiedad
de descriptores de fsd, permisos del fichero de clave, confirmación tardía
A/B, selección ante fallo de persistencia, limpieza de handles del broker,
límites y puesta a cero de memoria Wasm y ejecución de `start`.
No se reabren esos hallazgos en esta revisión.

El arranque USB en QEMU valida la imagen GPT/FAT32 y el cargador, pero emplea
un dispositivo virtio-blk. No demuestra soporte de almacenamiento USB,
AHCI/NVMe ni compatibilidad VT-d/PS/2 con un portátil concreto. Esas
limitaciones también están recogidas en `docs/hardware.md`.

## Resolución (rama `fix/audit-3eb4965`, 2026-09-28)

| # | Estado | Corrección | Regresión |
|---|--------|------------|-----------|
| 1 | Corregido | `Process.reaped`: `release_address_space` libera el espacio y el dominio DMA y solo entonces marca el proceso recogido y despierta a `exit_wait`; `wait_until` no retira el proceso hasta ver `reaped` (ya no consulta el estado del hilo ni cede en el hueco). Orden de locks intacto: `PROCESSES` nunca se toma dentro de `SCHED`. | `test=process_reap`: 180 procesos `hello` creados y esperados por lotes de 6, con recuento de frames del PMM (descontando el crecimiento del heap) y tabla de procesos vacía al final. Escenarios `user.process_reap` (1 CPU) y `smp.process_reap` (`cpus = 4`, la intercalación del hallazgo). |
| 3 (hallado al corregir) | Corregido | La consola de framebuffer (rejilla de 32 KiB) se construía por valor en la pila de 64 KiB de AriBoot y la desbordaba; con la imagen USB y `-smp 4` el initrd quedaba justo debajo y se corrompía (`initrd: imagen invalida (Truncated)`). `Console::empty()` es `const` y vive en el `static`; `attach` acopla la pantalla sin mover la rejilla. Además `check_boot_stack` mide la pila de arranque antes de abandonarla y exige 16 KiB de margen (pánico con diagnóstico en vez de corrupción), y AriBoot y el kernel anuncian el SHA-256 del initrd. | `usb.image_boot` con `--cpus 4` (antes fallaba), `empty_console_is_const_and_attaches_in_place`; toda la suite con `--cpus 4`. |
| 2 | Corregido | `State.owners`: un propietario único por función PCI. `attach` prepara la tabla de contexto y el dominio antes de anotar nada, escribe el contexto, transfiere la propiedad y retira la función de la lista del dominio anterior; `release` solo borra los contextos cuyo propietario sigue siendo ese dominio. | `iommu::selftest`: la función `00:02.0` se asigna al dominio 1001, se reasigna al 1002 y se libera el 1001: la entrada de contexto real (leída de las tablas con `context_domain`) sigue señalando al dominio de 1002; al liberar 1002 desaparece. Escenario `iommu.isolation`. |
