# ADR 0014: Persistencia de AriFS: sumas de control, journal de reemplazo y `flush`

- **Fecha:** 2026-09-28
- **Estado:** Aceptada

## Contexto

AriFS v0 escribía en el sitio sin sumas de control ni `flush`: un corte
durante una escritura podía dejar un fichero mezclado, un metadato roto
pasaba por bueno y nada garantizaba que lo escrito hubiera llegado al
soporte. El estudio de 2026-09-28 (etapa 2) pide un contrato explícito de
`flush`, escrituras parciales, reemplazo atómico y recuperación, y elegir
entre journal y copy-on-write con un ADR. A/B protege el sistema, no los
datos de las aplicaciones.

## Decisión

1. **Formato v1 con CRC32** en el superbloque y en cada entrada en uso de la
   tabla. `open` devuelve `Corrupt` si alguno no cuadra y fsd no monta: un
   sistema del que no se puede fiar no se sirve ni se "repara" borrando.
   No hay compatibilidad con v0 (era un prototipo; xtask reformatea).
2. **Journal de reemplazo** (un sector de cabecera y 64 de datos, entre la
   tabla y los datos) en lugar de copy-on-write: los ficheros tienen
   capacidad fija y sin fragmentación, así que un solo bloque de
   transición basta y el formato apenas cambia. `replace(fd, datos)`
   escribe los datos y su CRC en el journal, `flush`, marca la cabecera
   pendiente con su propio CRC, `flush`, copia al fichero, fija el tamaño,
   `flush`, limpia la cabecera, `flush`. Al montar, una cabecera pendiente
   válida con datos que cuadran se rehace (idempotente); una cabecera rota
   o unos datos que no cuadran se descartan: el reemplazo nunca llegó a
   comprometerse y el fichero conserva el contenido anterior.
3. **Contrato de `write`**: escribe en el sitio, sector a sector, y solo
   después sube el tamaño. Tras un corte cada sector tiene el contenido
   viejo o el nuevo; no hay garantía entre sectores. No es duradero hasta
   `flush`. Quien necesite todo-o-nada usa `replace`.
4. **`flush` de extremo a extremo**: `virtio-blk` negocia `VIRTIO_BLK_F_FLUSH`
   y atiende `Op::Flush` del protocolo de bloques (sin él, `Flush` vuelve
   sin hacer nada y el driver lo anuncia al arrancar); `BlockDevice::flush`
   lo lleva a la librería; el protocolo de ficheros gana `Flush` y `Replace`
   y el SDK `FsClient::{flush, replace}`.
5. **Supuesto del soporte**: un sector de 512 bytes se escribe entero o no
   se escribe. Sin ese supuesto ningún esquema con una sola copia de la
   cabecera es seguro; se documenta para el equipo de referencia.

## Alternativas descartadas

- **Copy-on-write con dos copias por fichero**: dobla el espacio de datos
  (invade el área en bruto del disco de pruebas) y sigue necesitando un
  puntero atómico; no aporta nada frente al journal con capacidad fija.
- **Journal general de escrituras parciales**: multiplica las escrituras de
  cada `write` y complica la reproducción; el caso de uso de la etapa
  (guardar un fichero entero) lo cubre `replace`.
- **Reparar al montar** (descartar entradas corruptas y seguir): oculta
  pérdidas; se prefiere fallar visible y dejar la decisión a quien opera.

## Consecuencias

- Los tests de host de `libs/arifs` cortan un reemplazo en cada escritura,
  con la caché perdida o conservada, y comprueban que el fichero queda
  entero (viejo o nuevo) y que el sistema sigue sirviendo. También cubren
  CRC de superbloque y tabla, journal roto y datos que no cuadran.
- `filesystem.replace_recovery` (disco `fs-pending-replace`) y
  `filesystem.corrupt_detected` (disco `fs-corrupt`) lo prueban en QEMU;
  `filesystem.read_write` exige el `flush` negociado y un `replace` por IPC.
- `replace` por IPC admite hasta `MAX_IO` (4 KiB) por petición: el tamaño de
  una nota, no de un módulo; las escrituras largas siguen siendo `write`.
