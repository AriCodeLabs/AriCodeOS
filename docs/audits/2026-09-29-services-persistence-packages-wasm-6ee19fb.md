# Revisión de supervisión, persistencia, paquetes y WASM

Fecha: 2026-09-29. Commit revisado: `6ee19fb`.
Comparación: `4836023..6ee19fb`; árbol limpio al comenzar.

Alcance: correcciones de USB/HID, versión 0.6.0 y etapas 1–4: supervisor,
correlación IPC, puente de consola, AriFS v1, actualizaciones entre arranques,
broker asíncrono, permisos por archivo, paquetes e instalador, validador WASM.

**Siete hallazgos abiertos: dos P1 y cinco P2.** Hay reproducciones de host
y QEMU. No se han aplicado correcciones al código de producción.

## Validación

La batería oficial se ejecuta en una copia de `git archive 6ee19fb`, en
`/var/tmp/aricode-review-6ee19fb-uhBqbF`, sobre disco, no en el tmpfs `/tmp`.
Como había otra batería ejecutándose en el repositorio, los puertos de prueba
17007–17010 se sustituyeron por 31007–31010 en constantes, URLs y expectativas
de esa copia. No se cambiaron algoritmos ni escenarios para obtener resultados.

| Comprobación | Resultado |
| --- | --- |
| `cargo xtask check` | Correcto |
| Batería oficial de host | 231 correctas, 0 fallidas |
| `cargo xtask test --all` | 100 escenarios: 96 correctos, 4 fallidos por expectativas de recuento (hallazgo 7); salida 1 |
| Sondas adicionales de host | 7 aserciones de regresión fallidas: 3 de persistencia, 3 de validación WASM y 1 de ejecución de loops |
| Referencia WASM con Node | Rechaza los tres módulos inválidos; el loop válido devuelve 0 |
| QEMU: journal, abandono de reemplazo e importación WASM | Los tres defectos reproducidos; el sistema continúa y se apaga con código 33 |
| QEMU: servicio que no responde al arrancar | Dos procesos siguen activos tras declararlos fallidos; las reclamaciones PCI posteriores fallan |

Las sondas adicionales no forman parte de los 231 tests oficiales. Sus fallos
son aserciones deliberadas del comportamiento esperado, no errores de compilación.
Evidencias y reproducción: [6ee19fb-review](6ee19fb-review/README.md).

## 1. [P1] El compromiso por trozos permite leer restos del journal de otra aplicación

Código: [services/arifs/src/main.rs](../../services/arifs/src/main.rs),
líneas 278–287 y 309–325;
[libs/arifs/src/lib.rs](../../libs/arifs/src/lib.rs), `replace_chunk` y
`commit_header`, líneas 479–539. Introducido por `9ce36fc`.

`Staging` solo guarda archivo y propietario. No registra qué bytes recibió la
transacción. `ReplaceCommit` acepta una longitud mayor que los trozos enviados,
y calcula el CRC de lo que haya en el journal global, incluidos restos de un
reemplazo anterior. Limitar la longitud a 32 KiB no comprueba su procedencia.

En QEMU, `reviewa` y `reviewb` son aplicaciones distintas, con directorios
privados y únicamente `caps = fs`. A reemplaza su archivo por una cadena sintética.
B envía un trozo de un byte a su propio archivo y compromete 22 bytes. B lee:

```text
review: A wrote private fixture
review: B read from its own file: XYNTHETIC_PRIVATE_DATA
```

B no recibió un archivo compartido ni el endpoint raíz. Los permisos de archivo
se respetan al abrir, pero los datos atraviesan el journal compartido.

Corrección propuesta: definir una transacción con longitud esperada y cobertura
de todos los bytes, o exigir trozos consecutivos desde cero; rechazar el compromiso
incompleto. Inicializar una transacción nueva sin reutilizar datos de otro dueño.
Regresión: dos namespaces, reemplazo previo y compromiso con huecos o longitud
excesiva; no debe aparecer ningún byte de la transacción anterior.

## 2. [P1] Faltan barreras antes de publicar metadatos dependientes de otras escrituras

Código: [libs/arifs/src/lib.rs](../../libs/arifs/src/lib.rs), líneas 400–403
y 451–458. La nueva persistencia y su contrato se incorporan en `61d141a`.

El contrato de `BlockDevice` permite reordenación de sectores hasta `flush`.
`create_sized` escribe el superbloque y la entrada sin barrera intermedia; el
comentario supone un orden duradero que la interfaz no garantiza. Si un corte
conserva la entrada pero pierde el avance de `data_next`, el siguiente montaje
acepta ambos metadatos con CRC válido y vuelve a asignar el mismo espacio.

Una sonda con caché de escritura, sectores atómicos y corte antes de `flush`
produce `first_lba=70, second_lba=70`. Dos archivos diferentes quedan solapados.
El mismo modelo aplicado a `write` conserva un tamaño de tres bytes sin conservar
sus datos: tras reabrir se leen `[0, 0, 0]`, aunque se escribió `NEW`.

Que una operación no sea todavía duradera permite perderla; no debe permitir
que la recuperación monte archivos solapados. Las pruebas existentes de cortes
en orden de escritura no cubren este caso de reordenación permitido por el contrato.

Corrección propuesta: persistir la reserva antes de publicar la entrada, y los
datos antes de publicar un crecimiento, o incluir esas operaciones en una
transacción recuperable. Comprobar también coherencia de rangos y asignación al
montar. Regresión: modelo de disco con caché y persistencia selectiva de sectores,
no solo un contador que interrumpa escrituras en su orden original.

## 3. [P2] Un cliente que termina a mitad del reemplazo deja ocupado el journal global

Código: [services/arifs/src/main.rs](../../services/arifs/src/main.rs),
líneas 267–285 y 309–324. Introducido por `9ce36fc`.

La reserva `srv.staging` solo se libera por cierre explícito del descriptor,
compromiso o determinados errores. No tiene plazo ni un mecanismo asociado a la
muerte del cliente. Los endpoints de namespace se conservan en fsd y se comparten
entre lanzamientos, por lo que la salida del proceso no dispara esa limpieza.

Reproducción QEMU: A envía el primer trozo y termina sin cerrar; B intenta un
reemplazo normal mediante `FsClient::replace`:

```text
review: exiting after chunk without commit/close
user: proceso 22 'reviewa' salio con codigo 0
review: BUSY after other app exited
```

Quedan afectados los reemplazos atómicos de otras aplicaciones, incluido el
instalador. No se afirma que todas las lecturas o escrituras del sistema se bloqueen.

Corrección propuesta: identidad de transacción y cancelación/recuperación al
perder al dueño, más un plazo acotado para una transacción abandonada. Rechazar
commits de transacciones caducadas. Regresión: terminar al cliente entre dos
trozos y comprobar que otro puede completar un reemplazo sin reiniciar fsd.

## 4. [P2] El supervisor pierde el proceso vivo cuando vence su sonda de arranque

Código: [services/init/src/main.rs](../../services/init/src/main.rs),
líneas 530–542. Introducido por `4607e5d`.

Cuando la sonda falla, `launch` llama a `announce_death(StartTimeout)`.
Esta función extrae y cierra el handle del proceso, pero no lo termina ni espera
su salida. `HANDLE_CLOSE` solo retira el handle: el proceso puede seguir ejecutándose
y conservar sus dispositivos. El supervisor programa otra generación y pierde
la referencia con la que vigilaba la anterior.

Reproducción QEMU con un fixture: sustituir el binario `virtio_input` del initrd
por el `cpu_hog` existente, que no responde y no termina. No se modifica init ni
el kernel. Tras vencer ambas sondas, aparecen `pci: ya reclamado` al reintentar;
`services --detail` muestra servicios parados mientras `ps` muestra los PID 14
y 15 de `virtio_input` todavía activos.

Corrección propuesta: distinguir «no responde» de «terminó». Conservar el handle
hasta confirmar salida y limpieza; si se añade terminación desde el supervisor,
esperar a que finalice antes de reclamar PCI. Si no se puede terminar, marcar el
servicio como fallido vivo y no lanzar otra instancia como si se hubiera liberado.
Regresión: arranque lento o permanentemente bloqueado con propiedad PCI retenida.

## 5. [P2] El runner resuelve importaciones WASM sin comprobar sus firmas

Código: [apps/wasm-runner/src/main.rs](../../apps/wasm-runner/src/main.rs),
líneas 319–345 y 142–143. Nuevas importaciones de archivos en `6ee19fb`.

La resolución comprueba módulo, nombre y capability, pero no compara
`imp.type_index` con la firma de la función anfitriona. El validador solo conoce
la firma declarada por el módulo; no puede comprobar por sí solo la API de Rust.
El anfitrión accede a `args[0..3]` suponiendo cuatro argumentos.

Un módulo válido de 66 bytes importa `aricode.file_write` como `() -> i32` y lo
invoca desde `main(i64) -> i32`. Pasa `Module::parse`, tiene `files` concedido y
en QEMU termina con `panic en programa de usuario`, código 255. La firma real
exige cuatro `i32`. Es un fallo del runner aislado, no un panic del kernel.

Corrección propuesta: asociar parámetros y resultados a cada `Api`, compararlos
al enlazar y rechazar la incompatibilidad antes de ejecutar `start`. Comprobar
también la firma de `main`; mantener comprobaciones defensivas de longitud en
el anfitrión. Regresión: imports con menos argumentos, tipos incorrectos y
resultados incompatibles; deben producir un error controlado de enlace.

## 6. [P2] La nueva validación deja pasar módulos estructuralmente inválidos

Código: [libs/wasm/src/validate.rs](../../libs/wasm/src/validate.rs),
líneas 490–527, especialmente la comprobación de exportaciones en 510–514.
Introducido como validación completa en `6ee19fb`.

Se validan cuerpos y determinados índices, pero se aceptan estos tres módulos
mínimos, conservados en las evidencias:

- `wrong-global.wasm`: global declarada `i32` inicializada mediante `i64.const`.
- `missing-memory.wasm`: exportación de memoria 99 sin ninguna memoria definida.
- `inverted-limits.wasm`: memoria con mínimo 2 y máximo 1.

Las tres sondas que esperan rechazo fallan; `WebAssembly.validate` de Node devuelve
`false` en las tres. El parser descarta parte de la información necesaria para
revalidar inicializadores y el validador solo comprueba exportaciones de función.
No basta con que la ejecución posterior pueda fallar: la nueva API afirma rechazar
el módulo antes de instanciarlo.

Corrección propuesta: conservar y validar tipos de expresiones constantes,
límites y todos los espacios de índices; completar también las comprobaciones
de estructura y del perfil anunciado. Regresión: corpus positivo/negativo
contrastado con otro motor. La [especificación de validación de módulos](https://webassembly.github.io/spec/core/valid/modules.html)
define estas obligaciones; las tres sondas usan características básicas, no
extensiones fuera del perfil.

## 7. [P2] La imagen nueva deja desactualizadas expectativas de la batería obligatoria

Código: [tools/xtask/src/image.rs](../../tools/xtask/src/image.rs), líneas
650–653; [filesystem_read_write.toml](../../tests/qemu/filesystem_read_write.toml),
línea 12, y [filesystem_replace_recovery.toml](../../tests/qemu/filesystem_replace_recovery.toml),
línea 18; [shell_write_persists.toml](../../tests/qemu/shell_write_persists.toml),
línea 28; [usb_image_boot.toml](../../tests/qemu/usb_image_boot.toml),
línea 19. Regresión de `6ee19fb`.

La imagen añade `wasm_runner/demo.manifest` junto al módulo, pasando de tres a
cuatro archivos iniciales. Cuatro escenarios siguen usando
los recuentos anteriores: tres al montar, o cinco tras crear archivos entre
arranques, en vez de cuatro y seis. Fallan `filesystem.read_write`,
`filesystem.replace_recovery`, `shell.write_persists` y `usb.image_boot`.
Alcanzan sus operaciones y finalizan, pero las expectativas rompen
`cargo xtask test --all`.

Corrección propuesta: actualizar los escenarios junto al fixture, o comprobar
los archivos necesarios sin acoplar pruebas de persistencia al número de archivos
WASM. Regresión: los escenarios afectados deben pasar con la imagen realmente construida.

## Correcciones de la revisión anterior

`fb60352` incorpora las cuatro correcciones de `4836023`: filtro PCI por interfaz
xHCI, sonda antes de publicar entrada, intervalos FS/LS corregidos, capacidad para
21 eventos HID y descriptor inicial de ocho bytes antes de ajustar EP0.
Las pruebas de host de HID/intervalos y los escenarios QEMU `usb.hid`,
`usb.hid_boot` y `usb.hid_boot_with_ehci` pasan en esta batería.
El fallo de `usb.image_boot` es la expectativa de recuento del hallazgo 7,
no una regresión de enumeración xHCI.
El caso físico FS con EP0 de 64 bytes sigue sin verificarse en hardware.

## Observación adicional: loops WASM con parámetros

La sonda `valid_loop_parameters_preserved_on_branch` valida un módulo cuyo loop
recibe un `i32`, decrementa un contador y usa `br_if` para repetir. Node lo acepta
y devuelve 0; AriCodeOS también lo valida, pero ejecutándolo devuelve
`Trap("pila vacia")`. `Instance::branch` conserva cero valores al saltar a un loop
en vez de sus parámetros, en [libs/wasm/src/lib.rs](../../libs/wasm/src/lib.rs),
líneas 657–665. Esa lógica ya estaba en `4836023`; se registra fuera de los siete
hallazgos nuevos, aunque afecta al perfil documentado ahora. Rechazar temporalmente
esa forma o implementar sus parámetros de etiqueta evitaría anunciarla como soportada.

## Límites y resolución

Estado de los siete hallazgos: **abiertos**. Este turno solo añade informe y
evidencias. No se han cambiado las ramas, creado commits ni alterado las pruebas
que otro proceso ejecutaba en el repositorio.

La revisión cubre el rango indicado, con mayor profundidad en persistencia,
supervisión, paquetes y WASM. No certifica ausencia de otros defectos, hardware
físico, toda la batería forzada a cuatro CPU ni resistencia a todos los fallos de
almacenamiento. Los escenarios `kill_on` de actualizaciones cortan al observar un
marcador anterior a las escrituras: no determinan por sí solos todos los puntos
intermedios del journal ni la reordenación de sectores.
