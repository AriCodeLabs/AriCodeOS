# ADR 0019: Perfil `aricode-wasm` v1: validación previa y manifiesto por módulo

- **Fecha:** 2026-09-29
- **Estado:** Aceptada

## Contexto

El intérprete (Fase 29) confiaba en el productor del módulo: comprobaba
límites en ejecución, pero un módulo mal tipado podía fallar a mitad de
camino, y la Capability API la decidía el runner sin que el módulo
declarase qué necesita. El estudio de 2026-09-28 (etapa 4, apartado H)
pide un validador, un manifiesto por módulo con recursos concedidos
separados de los privilegios del runner, ficheros y reloj, y un perfil
explícito comprobable fuera del sistema, sin dar por soportado WASI.

## Decisión

1. **Validación antes de instanciar.** `Module::parse` ejecuta el
   algoritmo de tipado de la especificación sobre cada función (pila de
   operandos tipada, marcos de control, código inalcanzable polimórfico) y
   comprueba índices, tabla, globales, exportaciones y `start`. Lo que no
   valida no se instancia (`Error::Invalid`); lo que el intérprete no
   ejecuta (coma flotante, SIMD, otras extensiones) se rechaza aquí, no
   con un trap a mitad de ejecución.
2. **Perfil propio y explícito** (`docs/wasm-profile.md`): MVP con enteros,
   `sign-ext`, `memory.copy`/`memory.fill`, `select t`, tipos de bloque por
   índice; una tabla, una memoria, importaciones solo del módulo
   `aricode`. No se afirma WASI: un módulo WASI no funciona y no se
   pretende que lo haga.
3. **Manifiesto por módulo** (`NOMBRE.manifest`, `caps = print, clock,
   files`): el runner concede una importación solo si el manifiesto pide
   la capacidad que la cubre; lo demás no existe para el módulo, aunque el
   runner tenga el recurso. Los recursos del módulo son un subconjunto de
   los del runner (su directorio privado, su reloj), nunca más.
4. **Ficheros y reloj** en la Capability API: `file_write` (reemplazo
   atómico en el directorio privado del runner) y `file_read`, con nombres
   cortos sin `/`, más `uptime_ms` y `print`. Las llamadas que hablan con
   un servicio heredan los plazos del SDK (ADR 0012).

## Alternativas descartadas

- Integrar un runtime externo (wasmtime, wasmi): licencia, tamaño y
  `no_std` no encajan hoy con un kernel y un SDK que caben en unos cientos
  de KiB; el validador propio cubre el subconjunto que se ejecuta.
- WASI preview 1 "mínimo": exige un modelo de descriptores y errno que el
  sistema no tiene, y prometería compatibilidad que no se puede cumplir.
- Manifiesto dentro del módulo (sección custom): más difícil de leer y de
  cambiar sin recompilar; el fichero al lado es lo mismo que hacen las
  aplicaciones nativas.

## Consecuencias

- `wasm.demo` valida y ejecuta el módulo real de `rustc`, que escribe y
  relee un fichero; `wasm.restricted` (manifiesto solo `print`) no ejecuta
  por una importación no concedida; `wasm.invalid` rechaza un módulo mal
  tipado con el motivo.
- Los tests de host de `aricode-wasm` cubren pila vacía, tipos, valores
  sobrantes, etiquetas, `if` sin `else` con resultados, índices fuera de
  rango, opcodes desconocidos y código inalcanzable polimórfico.
- Ampliar el perfil (coma flotante, más API) pasa por el validador y por
  esta página; el perfil se versiona.
