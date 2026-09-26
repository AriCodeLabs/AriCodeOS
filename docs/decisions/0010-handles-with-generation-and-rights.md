# ADR 0010: Objetos del kernel por handle con generación y derechos

- **Fecha:** 2026-09-26
- **Estado:** Aceptada

## Contexto

La sección 24 del documento maestro exige que las aplicaciones no manejen
punteros ni identificadores globales de objetos del kernel, y la sección 25
que el acceso dependa de una capability explícita.

## Decisión

Cada proceso tiene una tabla de handles (`libs/handles`) con límite fijo. Un
handle es un `u32` opaco con índice (16 bits) y generación (16 bits); `0`
nunca es válido. Cada entrada lleva un `Arc<KernelObject>` y unos `Rights`.
Las reglas:

1. Un handle cerrado no vuelve a ser válido aunque su índice se reutilice
   (la generación cambia).
2. Un handle inventado solo sería válido si acertara índice y generación de
   una entrada viva.
3. Duplicar exige el derecho `DUPLICATE` y solo puede reducir derechos.
4. Cada syscall que opera sobre un objeto declara el derecho que exige
   (`console_write` -> `WRITE`, `object_id` -> `READ`).
5. Los identificadores informativos (tid, pid) que devuelve `object_id` no
   dan acceso: solo el handle lo da.

Todo proceso arranca con tres handles fijos: su proceso, su hilo principal y
la consola (`sdk/abi::handle`).

## Alternativas

- Identificadores globales (números de hilo, de proceso) como argumento de
  syscall: cualquiera podría nombrar cualquier objeto; descartado.
- Handles sin generación: un handle cerrado y reasignado daría acceso a un
  objeto ajeno; descartado.

## Consecuencias

- El ABI pasa a versión 2: `console_write` recibe un handle.
- La Fase 13 (capabilities) extiende este modelo con delegación entre
  procesos (`TRANSFER`) y revocación, sin cambiar la representación.
- Las tablas viven en el kernel y cuentan contra el límite del proceso
  (sección 52).
