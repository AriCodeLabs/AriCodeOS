# Revisión de sandbox, WebAssembly, criptografía y arranque A/B

Fecha: 2026-09-27. Base Git: `e9fdf38`.
Alcance: cambios posteriores a `28e745a`, incluidas las fases 28, 29 y 31,
las correcciones de la auditoría anterior y el trabajo de Fase 30 sin commit
presente al iniciar esta revisión (versión de workspace 0.4.0).

La revisión se hizo sobre una copia aislada. Los hashes de los archivos
señalados están en [snapshot.json](e9fdf38-review/snapshot.json): citar solo
`e9fdf38` no reproduce los cambios A/B que aún no estaban confirmados.
No se modificó código de producción ni se aplicaron correcciones.

Durante la revisión, Fase 30 se integró en `ade2d04` (implementación `ab4af73`).
Los siete archivos de producción identificados en `snapshot.json` mantienen
exactamente los mismos hashes en esa integración.

## Hallazgos

| # | Prioridad | Problema | Estado |
| --- | --- | --- | --- |
| 1 | P1 | Un descriptor global permite acceder a archivos de otro directorio privado | Pendiente |
| 2 | P1 | `keygen` crea la semilla privada con permisos de lectura para otros usuarios | Pendiente |
| 3 | P1 | Se confirma un slot antes de completar el arranque de los servicios | Pendiente |
| 4 | P2 | Se arranca un slot pendiente aunque falle la persistencia del contador | Pendiente |
| 5 | P2 | Las denegaciones previas a `process_spawn_limited` todavía pierden handles | Pendiente |
| 6 | P2 | Wasm comprueba los accesos contra la reserva del host, no contra la memoria actual del módulo | Pendiente |
| 7 | P2 | Wasm conserva bytes anteriores al instanciar y al ampliar la memoria | Pendiente |
| 8 | P2 | El runner ignora la función `start` de los módulos Wasm | Pendiente |

P1 requiere atención prioritaria; P2 es un defecto funcional que debe corregirse.
La revisión no constituye una verificación formal de las primitivas criptográficas
ni una auditoría exhaustiva de todo el sistema operativo.

## Validación y evidencias

- `cargo xtask check`: correcto en la copia original, antes de añadir las
  reproducciones y adaptar los puertos de prueba.
- Pruebas de host de la batería: 184 correctas.
- Resultado QEMU consolidado tras las repeticiones: **65 escenarios oficiales
  correctos**. Los dos escenarios nuevos, `review.fs_isolation` y
  `review.broker_leak`, fallan de forma reproducible y confirman los hallazgos
  1 y 5. El consolidado y los resultados individuales se conservan en
  `review-results.json`; no se presenta la ejecución inicial como una pasada
  completa sin errores.
- La primera ejecución de QEMU recibió SIGTERM mientras había otra batería
  activa en el workspace. No se considera una ejecución completa ni un fallo
  funcional de AriCodeOS.
- La segunda ejecución utiliza los puertos 27007–27010 y nombres diferentes
  para el runner y QEMU. Se conservan las mismas rutas de producción de los
  hallazgos; solo se adapta la infraestructura de pruebas y se añaden dos
  escenarios de reproducción.
- Se repitieron los escenarios afectados al completar la adaptación de las
  URLs HTTP de test. La expectativa de capacidad de `virtio.blk_rw` se tomó de
  `ade2d04`: durante la revisión se corrigió de 8192 a 98304 sectores, acordes
  con el disco A/B. Estos fallos de infraestructura y expectativa no se cuentan
  entre los hallazgos pendientes.
- Las cuatro aserciones adicionales de Wasm fallan con los resultados
  descritos en los hallazgos 6–8. Ejecutan directamente `aricode-wasm`.
- La prueba aislada de archivos obtiene `SECRET` desde otro prefijo y falla
  su aserción de aislamiento. Usa el handler extraído, AriFS y el protocolo
  originales; simula el disco y tres consultas DMA.
- Las reproducciones A/B ejecutan QEMU sobre discos propios: dos arranques
  en solo lectura y un arranque con red restringida, respectivamente.
- La prueba de `keygen` usa un directorio nuevo y `umask 022`; no se conserva
  ni se muestra la semilla generada.

Los logs locales están en `build/audit/e9fdf38-worktree/` y las instrucciones
y fuentes de las reproducciones en [e9fdf38-review/](e9fdf38-review/README.md).
Los logs y discos son artefactos generados, no archivos versionados.

## 1. [P1] Los descriptores de fsd no están ligados al directorio privado

Código: [services/arifs/src/main.rs](../../services/arifs/src/main.rs),
líneas 172–195; tabla única creada en `main`, línea 224 de la copia revisada.

`Open`, `Create` y `List` aplican el prefijo del endpoint, pero `Read`, `Write`
y `Close` consultan la misma tabla `Fds` para todos los endpoints. El número
de descriptor no incluye el directorio propietario y no se comprueba que el
archivo pertenezca al prefijo que envía la petición.

Una aplicación con su propio endpoint privado puede probar los descriptores
0–15 y leer, modificar o cerrar archivos abiertos desde otro directorio.
También funciona después de que el propietario termine sin cerrar su descriptor,
porque fsd conserva esa entrada. No hace falta obtener el endpoint de la víctima.

Reproducción de host: crear `victim/secret.txt`, escribir `SECRET`, conservar
el descriptor 0 y enviar `Read(fd=0)` con prefijo `attacker`. Resultado:
`Status::Ok` y los seis bytes `SECRET`, frente al rechazo esperado.

La reproducción QEMU usa dos aplicaciones con manifiestos `caps = fs` y
directorios distintos. La primera escribe `VICTIM_SECRET` y termina sin
`Close`; la segunda intenta leer los descriptores 0–15 sin abrir ese archivo.
Resultado observado: `REVIEW_CROSS_NAMESPACE_READ: VICTIM_SECRET` y salida 1
del atacante. El escenario falla las expectativas de aislamiento.

Corrección: asociar cada descriptor al namespace o mantener una tabla por
namespace y validar pertenencia en todas las operaciones, incluido `Close`.
La regresión debe intentar lectura, escritura y cierre cruzados; comprobar
solo `List` o `Open` por nombre no cubre este acceso.

## 2. [P1] La semilla privada se escribe con permisos demasiado amplios

Código: [tools/xtask/src/keys.rs](../../tools/xtask/src/keys.rs), líneas 32–35
y 48–52.

`keygen` usa `write_hex`, que termina en `fs::write`, tanto para la clave
pública como para la semilla privada. No establece permisos privados sobre
esta última. Con `umask 022`, la semilla se crea con modo `0644`; el directorio
nuevo también es accesible bajo esa configuración si sus ancestros lo permiten.

Reproducción: ejecutar `xtask keygen` en un directorio temporal nuevo bajo
`umask 022`. Resultado: `ed25519.seed = 0644`, `ed25519.pub = 0644`, salida 0.
No se trata de la clave de desarrollo publicada deliberadamente en `keys/dev`,
sino de las nuevas claves privadas que genera el comando.

Impacto: otros usuarios locales que puedan recorrer el directorio pueden
copiar la semilla y firmar como su propietario.

Corrección: crear la semilla con permisos `0600` desde su apertura y creación
exclusiva. No depender del umask ni de un `chmod` posterior a escribirla.
Regresión: comprobar que no existen bits de acceso para grupo u otros bajo
un umask permisivo y que una clave existente no se sobrescribe.

## 3. [P1] Init confirma el slot antes de saber si completa el arranque

Código: [services/init/src/main.rs](../../services/init/src/main.rs),
líneas 604–608.

`confirm_boot_slot` se llama después de comprobar bloques y archivos, pero
antes de iniciar y comprobar red, entrada, GUI y broker. Aún hay múltiples
rutas posteriores que terminan init con un error. La confirmación ya ha
convertido el slot pendiente en activo y eliminado el mecanismo de reintentos.

Reproducción QEMU: preparar A activo y B pendiente con tres intentos y arrancar
con una red de usuario restringida. Se observa, en este orden:

```text
AriInit: arranque confirmado en el slot B
net_client: ip 10.0.2.15 router 0.0.0.0 dns 0.0.0.0
AriInit: net_client salio con 4
init terminado con codigo 18
```

El sector ARIBOOT queda con `active = B`, `pending = None`, intentos 0.
Por tanto, los siguientes arranques ya no pueden agotar los intentos de ese
slot y volver automáticamente a A por este fallo de inicialización. La sonda
provoca una configuración de red inválida; la misma ruta afecta a una imagen
nueva cuyo servicio de red falle después de superar las pruebas de almacenamiento.

Corrección: confirmar únicamente después de superar todos los requisitos de
salud del arranque solicitado, justo antes de entregar el control al shell o
de declarar el sistema listo. Mantener el estado pendiente en las salidas de
error. Añadir una regresión que compruebe el sector tras un fallo tardío de init.

## 4. [P2] Un fallo al guardar los intentos no impide arrancar el pendiente

Código: [boot/src/slots.rs](../../boot/src/slots.rs), líneas 164–184.

Cuando `disk.write` falla al persistir `decision.state`, AriBoot solo imprime
un aviso y devuelve la imagen elegida. Si era un slot pendiente, arranca aunque
el decremento de intentos no haya quedado guardado.

Reproducción QEMU: presentar el disco con `readonly=on`, A activo y B pendiente
con tres intentos. Ambos arranques consecutivos muestran:

```text
slots: estado activo A, pendiente B (3 intentos)
slots: no se pudo escribir el estado de arranque
slots: arrancando desde el slot B
```

Los dos arranques usan `test=random` para terminar sin confirmación desde init.
Esto prueba que el contador no avanza; no simula un fallo del kernel de B.
Si B no consigue arrancar, esta ruta puede repetir indefinidamente el mismo
slot en vez de alcanzar el rollback.

Corrección: no arrancar un pendiente si no se pudo guardar su intento. Volver
al slot activo verificado o a la recuperación, sin hacer depender esa elección
de que la escritura que acaba de fallar vuelva a funcionar. Una regresión debe
cubrir fallo de escritura y de flush del estado.

## 5. [P2] Persisten fugas del broker en los retornos anteriores al spawn

Código: [services/init/src/main.rs](../../services/init/src/main.rs),
líneas 366–395, especialmente el retorno de `WINDOW` sin compositor.

La corrección anterior cierra `granted` si falla `process_spawn_limited`,
pero hay retornos después de conceder algunos handles y antes de llegar a
esa llamada. Por ejemplo, con `caps = system, window` se duplica `SYSTEM` y
después se devuelve `Denied` si el arranque no incluye GUI. Ese duplicado no
se libera. Las rutas de error al preparar otros servicios tienen el mismo patrón.

Reproducción QEMU: un manifiesto `review_window` que pide `system, window`,
arranque `qemu_exit shell`, `repeat 300 run review_window` y después
`run hello_app 7`. No se necesita un ELF para el primer nombre: la denegación
ocurre antes de intentar cargarlo.

Resultado observado: tras las denegaciones aparece `el broker no pudo lanzarlo`
y `hello_app` ya no llega a ejecutarse. El escenario falla la expectativa de
su salida correcta.

Impacto: las denegaciones consumen la tabla de handles de init hasta impedir
otros lanzamientos. La regresión existente `repeat 300 run no-existe` no cubre
esta ruta: sus errores sí llegan a la limpieza añadida alrededor del spawn.

Corrección: usar limpieza común o un guard para todos los handles preparados
durante `launch`, incluidos los retornos por servicios ausentes y fallos al
duplicar. Desactivar esa limpieza únicamente cuando se haya transferido su
propiedad. Añadir el caso de denegación tras una concesión parcial.

## 6. [P2] Wasm accede más allá del tamaño actual de su memoria lineal

Código: [libs/wasm/src/lib.rs](../../libs/wasm/src/lib.rs), líneas 907–919;
afecta también a segmentos de datos, operaciones bulk y memoria entregada al host.

`memory_pages` registra el tamaño actual, pero las cargas y escrituras reciben
el slice completo proporcionado por el anfitrión. En el runner ese buffer tiene
cuatro páginas, aunque el módulo declare una sola. Un máximo declarado de una
página no limita estos accesos: solo interviene al ejecutar `memory.grow`.

Reproducción directa: módulo con mínimo y máximo de una página, buffer anfitrión
de cuatro páginas y `i32.load8_u` en dirección 65536. Con `0x5a` en esa posición,
devuelve `Ok(90)` en vez de un trap. No se ha observado acceso fuera del slice
de Rust ni fuera del proceso: se elude el límite de memoria del módulo.

Corrección: comprobar accesos e inicialización contra
`memory_pages * PAGE`, reservando la capacidad total del buffer para decidir
si puede crecer. Entregar también a los imports únicamente la memoria activa.
La especificación exige trap cuando un acceso supera el tamaño actual de la
memoria: [WebAssembly Core 1.0](https://webassembly.github.io/spec/versions/core/WebAssembly-1.0.pdf).

## 7. [P2] La memoria inicial y la ampliada no se ponen a cero

Código: [libs/wasm/src/lib.rs](../../libs/wasm/src/lib.rs), líneas 496–505
y 934–936.

`Instance::new` copia segmentos de datos sin limpiar el resto de la memoria.
`memory.grow` solo modifica el contador de páginas. Si el buffer anfitrión
contiene datos de un uso anterior, el módulo los recibe como memoria nueva.

Dos pruebas directas con un buffer relleno de `0xaa` devuelven `Ok(170)`:
una lee el byte 0 tras instanciar y otra lee el primer byte de la página recién
añadida. Ambas esperan cero. En el runner actual los buffers DMA nuevos se
inicializan a cero; el defecto está en el contrato reutilizable de la biblioteca
y se manifiesta al usar memoria preexistente o reutilizarla entre instancias.

Corrección: limpiar la memoria activa antes de aplicar los segmentos y poner
a cero cada región añadida al crecer. Mantener las dos regresiones con buffers
no vacíos. La inicialización a cero forma parte de la
[asignación de memoria de WebAssembly](https://webassembly.github.io/spec/core/exec/modules.html#alloc-mem).

## 8. [P2] La función start se analiza, pero no se ejecuta

Código: [apps/wasm-runner/src/main.rs](../../apps/wasm-runner/src/main.rs),
líneas 151–160; `Module::parse` conserva el índice en `module.start`.

Después de crear `Instance`, el runner llama directamente a `main` sin invocar
`module.start`. La biblioteca tampoco la ejecuta durante la instanciación.
Un módulo que depende de esa inicialización se acepta y ejecuta con estado
incorrecto; un trap de su función de inicio también queda ignorado.

Reproducción directa del mismo flujo: un módulo tiene una global inicializada
a 0, una función `start` que escribe 42 y una exportación que devuelve la global.
El resultado es `Ok(0)` en vez de `Ok(42)`.

Corrección: ejecutar `start` una sola vez después de inicializar memoria/tablas
y resolver imports, propagando los traps antes de ejecutar exportaciones; o
rechazar explícitamente estos módulos mientras no se soporte. La función de
inicio debe invocarse durante la
[instanciación de WebAssembly](https://webassembly.github.io/spec/core/exec/modules.html#instantiation).

## Seguimiento

Las dos correcciones de la [auditoría anterior](2026-09-27-sdk-28e745a.md)
están presentes en esta copia: enumeración que salta huecos y limpieza tras
fallos del spawn. El hallazgo 5 identifica una ruta anterior al spawn que esas
correcciones no cubrieron.

Los ocho hallazgos quedan pendientes de corrección y regresión en ramas
`fix/*`, según [WORKFLOW.md](../WORKFLOW.md). Esta revisión no crea commits,
no integra ramas y no cambia los archivos de producción del workspace.

## Resolución (rama `fix/audit-e9fdf38`, 2026-09-27)

| # | Estado | Corrección | Regresión |
|---|--------|------------|-----------|
| 1 | Corregido | fsd guarda en cada descriptor el endpoint que lo abrió; `Read`, `Write` y `Close` exigen que coincida con el que pregunta (`BadFd` si no). Dos lanzamientos de la misma aplicación comparten directorio y descriptores por diseño (misma identidad). | `sandbox.manifest`: `fs_victim` deja un descriptor abierto con `VICTIM_SECRET`; `fs_probe`, desde otro directorio, exige `BadFd` en lectura, escritura y cierre de los 16 descriptores y que su propio directorio siga funcionando. |
| 2 | Corregido | `keygen` escribe la semilla con `OpenOptions` (`create_new` + `mode(0o600)`) desde su creación; la clave pública sigue siendo legible. | `keys::tests::keygen_sign_verify_round_trip` comprueba que la semilla no tiene bits de grupo ni de otros. |
| 3 | Corregido | `confirm_boot_slot` se ejecuta al final de la secuencia, tras red, TCP, HTTP, entrada y GUI; cualquier retorno de error anterior deja el estado pendiente. | `update.no_confirm_on_late_failure`: `init_fail_late` (solo tests, vía cmdline) hace fallar a init justo antes de confirmar; el estado releído sigue `pendiente B (2 intentos)` y no hay confirmación. |
| 4 | Corregido | Si no se puede persistir el intento de un slot pendiente, AriBoot no lo arranca: `aricode-ab::unpersisted` vuelve al activo conservando el pendiente. | `update.readonly_no_pending_boot` (`disk_readonly = true`, nuevo campo de escenario): con B pendiente y disco de solo lectura arranca A. |
| 5 | Corregido | `Granted` (guard con `Drop`) cierra todos los duplicados preparados en `launch` salvo que `take` los haya entregado al kernel; cubre las denegaciones por servicios ausentes y los fallos al duplicar. | `sandbox.manifest`: `repeat 300 run win_probe` (manifiesto `system, window` sin GUI, denegado tras conceder SYSTEM) seguido de `run hello_app 10`, que debe funcionar. |
| 6 | Corregido | Cargas, almacenamientos, `memory.copy`/`fill`, segmentos de datos y la memoria que ven las importaciones se acotan a `memory_pages * PAGE`; el resto del buffer del anfitrión solo sirve para crecer. | `memory_is_bounded_by_current_pages_not_host_buffer`. |
| 7 | Corregido | `Instance::new` pone a cero la memoria activa antes de los segmentos de datos y `memory.grow` limpia las páginas nuevas. | `memory_starts_and_grows_zeroed` (buffers rellenos de `0xAA`). |
| 8 | Corregido | `Instance::start` ejecuta la función `start` (propagando traps); `wasm_runner` la llama antes de `main`. | `start_function_runs_before_exports` (global fijada por `start`; `start` con trap). |
