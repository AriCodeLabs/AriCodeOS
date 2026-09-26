# Revisión de la documentación de arquitectura

Commit revisado: `3a321d7` (0.0.5, fases 11 y 12).
Documento: [architecture.md](../architecture.md).
Comparación de cambios: `9d8c7e5..3a321d7`, más lectura completa del documento
actual para detectar contradicciones entre secciones.

Revisión documental terminada. Se contrastaron las afirmaciones siguientes con
el código del mismo commit. No es una auditoría exhaustiva de las fases 8–12
ni una validación de su ejecución. Las líneas citadas corresponden a ese commit.

## Hallazgos

### 1. [P2] El orden de locks publicado contradice el anidamiento real

Documento: línea 179, `heap -> pmm -> vmm -> console`.

En [vmm.rs](../../kernel/src/memory/vmm.rs), líneas 166–168, `with` mantiene
`VMM.lock()` durante el callback. Si el mapper necesita una tabla, llama a
`PmmFrames::alloc_frame` (líneas 33–34), que toma el PMM. El anidamiento es
por tanto VMM -> PMM. En el crecimiento del heap, la asignación previa de un
frame mediante PMM termina antes de entrar al VMM: la sucesión de llamadas
no implica mantener ambos locks en ese orden.

Seguir la regla publicada y llamar al VMM con el PMM tomado puede provocar
reentrada sobre PMM cuando haga falta una tabla. No se ha reproducido un
bloqueo actual: el hallazgo es que la regla documental induce un uso incorrecto.
El mismo comentario obsoleto aparece en `kernel/src/sync.rs:5–6`.

Corrección propuesta: documentar los locks realmente anidados, distinguir
el lock de crecimiento del lock del allocator y prohibir entrar al VMM
manteniendo el PMM. Revisar también el comentario de `sync.rs`.

### 2. [P2] Se mantienen afirmaciones de fases antiguas como estado vigente

Documento: líneas 41, 106–108, 189–190 y 295–297.

Se afirma que existe `SingleCpu`, que no hay APIC, que las interrupciones
externas están deshabilitadas, que la consola carece de lock y que
`time::sleep_ms` espera con `hlt` sin scheduler. Esto contradice las secciones
de sincronización, interrupciones y scheduler del propio documento.

Evidencias del código actual:

- [main.rs](../../kernel/src/main.rs), líneas 109–111: inicializa APIC y habilita interrupciones.
- [console.rs](../../kernel/src/console.rs), líneas 15 y 44: `IrqLock` y `try_lock`.
- [sync.rs](../../kernel/src/sync.rs), línea 33: alias `IrqLock`; ya no define `SingleCpu`.
- [time.rs](../../kernel/src/time.rs), líneas 27–28: delega el sueño en el scheduler.
- [exceptions.rs](../../kernel/src/arch/x86_64/exceptions.rs), líneas 12–20:
  #BP continúa; las demás excepciones de usuario terminan el proceso;
  las de kernel son fatales; los vectores externos se despachan al APIC.

Corrección propuesta: actualizar las secciones antiguas al estado 0.0.5 y
eliminar de la deuda las funcionalidades ya implementadas. Si se desea
conservar historia, marcarla expresamente como comportamiento de una versión pasada.

### 3. [P2] El selector de TSS ya no es 0x18

Documento: línea 99.

[aricode-x86-cpu](../../libs/x86-cpu/src/lib.rs), líneas 159–161, define los
selectores de usuario y `TSS = 0x28`. [gdt.rs](../../kernel/src/arch/x86_64/gdt.rs)
inserta datos y código de usuario antes del descriptor TSS y comprueba el layout.
La entrada GDT de índice 3, cuyo desplazamiento es 0x18, corresponde ahora a
datos de usuario, no al TSS.

Corrección propuesta: publicar el layout completo actual, incluidos los
selectores de usuario con RPL 3, y mantener una sola referencia para las
secciones de CPU y modo usuario.

### 4. [P2] El contrato de paginación describe una API retirada

Documento: líneas 60–62.

Se describe un `PagePool` inexistente y se afirma que la única operación
`unsafe` para el llamador es tomar posesión de su región. La implementación
actual en [aricode-x86-paging](../../libs/x86-paging/src/lib.rs) usa
`Mapper<A: FrameAlloc, M: PhysMap>` (línea 126), un contrato `unsafe PhysMap`
(línea 47) y una operación `unsafe share_kernel_half` (línea 346) que permite
compartir tablas creadas por otro mapper.

Corrección propuesta: sustituir la descripción histórica por los contratos
de los proveedores de frames y de traducción física, y explicar la validez
y vida útil exigidas a las tablas compartidas. No atribuir al mapper la
propiedad exclusiva de todas las tablas que recorre.

### 5. [P3] La lista de crates identifica BootInfo como v2

Documento: línea 28.

[boot-protocol](../../boot-protocol/src/lib.rs), línea 14, fija
`BOOT_PROTOCOL_VERSION = 3`; el diagrama y la sección del contrato ya dicen v3.
Corrección propuesta: actualizar la lista de crates a v3.

### 6. [P3] Se confunde la duración admitida con el error del test de sueño

Documento: línea 229, «50 ms con 5..7 ticks de error».

[scheduler.rs](../../kernel/src/task/scheduler.rs), líneas 374–382, compara
la duración total con `5..=7`. A 100 Hz son 50–70 ms totales; no una tolerancia
de 50–70 ms adicionales.

Corrección propuesta: escribir «50 ms, con una duración observada de 5–7 ticks».

### 7. [P3] La deuda sobre invalidación omite las pilas de hilo

Documento: línea 294, «solo el heap y la autoprueba invalidan».

[stack.rs](../../kernel/src/task/stack.rs), líneas 54–59, desmapea cada página
de la pila, ejecuta `invlpg` y devuelve el frame al PMM.

Corrección propuesta: incluir esa ruta y separar la invalidación local ya
implementada de la ausencia de un mecanismo genérico de TLB shootdown.

## Observaciones menores

- Línea 154: `Vec`, `Box`, `String` y `format!` vienen de la crate `alloc`;
  `core::alloc` proporciona interfaces como `Layout` y `GlobalAlloc`.
- Líneas 280–281: se presenta la fase 12 como futura aunque el documento
  declara que está completada. Mantener la limitación de un hilo por proceso
  y ausencia de `mmap`, pero ajustar la referencia del roadmap sin inventar
  una fase de implementación.

## Verificación y resolución

- Árbol de trabajo limpio al comenzar; HEAD `3a321d7`.
- Evidencias obtenidas por lectura del diff y contraste con el código actual.
- No se ejecutaron compilaciones ni QEMU: esta revisión es documental.
  Los resultados de tests de auditorías anteriores no se presentan como
  validación de este commit.
- Las correcciones del heap descritas en la resolución de la
  [auditoría anterior](2026-09-26-audit-9d8c7e5.md) están presentes por inspección:
  aritmética comprobada, reversión de páginas y préstamos separados mediante locks.
  Sus tests no se han vuelto a ejecutar en esta revisión.
- Los siete hallazgos y las dos observaciones quedan pendientes de corrección.
  No se ha editado `architecture.md` ni código de producción.
- Informe creado en `fix/audit-architecture-3a321d7`, sin commit ni merge.

---

# Resolución (rama `fix/audit-architecture-3a321d7`)

| # | Hallazgo | Corrección |
|---|----------|------------|
| 1 | Orden de locks incorrecto | La sección Sincronización publica los anidamientos reales verificados en el código (`VMM -> PMM`, `heap.mapped -> VMM/PMM`, `PROCESSES -> SCHED`, resto hoja), distingue el lock de crecimiento del lock del allocator y enumera las prohibiciones derivadas. El comentario de `kernel/src/sync.rs` dice lo mismo. |
| 2 | Estado de fases antiguas | Sección CPU: excepciones de usuario y despacho al APIC. Sección PMM: las regiones `Boot` las devuelve el VMM. Sección Fase 8: `time::sleep_ms` delega en el planificador. Deuda: eliminadas las entradas de APIC ausente y consola sin lock; lista de crates sin `SingleCpu`. |
| 3 | Selector de TSS | Sección CPU con el layout completo (0x08, 0x10, 0x18/0x1B, 0x20/0x23, 0x28) y remisión a `aricode-x86-cpu::layout` como única referencia. |
| 4 | Contrato de paginación | Sustituido `PagePool` por los contratos de `FrameAlloc` y `PhysMap` (trait `unsafe`) y la validez exigida a las tablas compartidas por `share_kernel_half`. |
| 5 | BootInfo v2 | Lista de crates: v3. |
| 6 | Tolerancia del sueño | «duración observada de 5 a 7 ticks (50 a 70 ms en total)». |
| 7 | Invalidación de TLB | Deuda reescrita: invalidación local explícita en heap, pilas de hilo y autoprueba; sin mecanismo genérico ni shootdown hasta SMP. |
| obs. | `alloc` vs `core::alloc` | Corregido en la sección Heap. |
| obs. | Fase 12 como futura | Deuda: `memory_map`/hilos de usuario remiten a la sección 22 y a las Fases 14 y 27. |

Sin cambios de código de producción más allá del comentario de `sync.rs`.
