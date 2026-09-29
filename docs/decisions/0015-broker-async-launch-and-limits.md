# ADR 0015: Lanzamientos asíncronos del broker y límites observables

- **Fecha:** 2026-09-28
- **Estado:** Aceptada

## Contexto

El broker (init) ejecutaba cada `run` de forma síncrona: `launch` esperaba
la salida del programa sin plazo. Mientras tanto init no vigilaba a los
servicios residentes (ADR 0011) ni atendía otras peticiones: un programa
que no termina bloqueaba la supervisión entera. El estudio de 2026-09-28
(etapa 3, apartado D) pide contadores y rechazo predecible en la
asignación de recursos y que otra aplicación siga progresando bajo carga.

## Decisión

1. **Lanzamientos asíncronos.** El broker lanza el programa y guarda en una
   ranura `(proceso, endpoint de respuesta)`; su bucle comprueba las
   ranuras con `process_wait(0)` y responde (código de salida o `Killed`)
   cuando el programa termina. `run` en el shell sigue esperando la
   respuesta, pero init ya no se detiene.
2. **Lanzamiento en segundo plano** (`Request::DETACH`, `sys::launch::spawn`,
   `spawn PROG [ARG]` en el shell): el broker responde `Started` en cuanto el
   proceso existe y cierra su handle cuando termina; el shell no espera.
3. **Límite de programas en marcha** (`MAX_RUNNING = 4` ranuras): un
   lanzamiento de más recibe `Status::Limit`, predecible y sin efectos. Es
   la primera cuota del broker; las del kernel siguen siendo la tabla de
   handles (256 por proceso, `Limit`), la cola de cada endpoint (16
   mensajes, espera acotada) y la memoria DMA del manifiesto.
4. **Sin prioridades**: el planificador por turnos reparte la CPU entre los
   hilos listos; con una CPU y cuatro programas que la consumen, el shell y
   una aplicación normal siguen progresando (`limits.hogs`). Presupuestos de
   CPU y prioridades quedan para una etapa posterior con medidas.

## Consecuencias

- `run` ya no bloquea la supervisión: un programa colgado no impide
  reiniciar un driver muerto.
- `limits.hogs` (una CPU): tres `cpu_hog` en segundo plano; en la cuarta
  ranura `handle_hog` agota su tabla (252 endpoints) y la libera y
  `hello_app` se lanza, escribe en fsd y termina; el cuarto hog ocupa la
  última ranura y el quinto se rechaza; `ps` muestra los hogs.
- Las ranuras las consumen también los programas que se ejecutan con `run`:
  con las cuatro ocupadas por programas en segundo plano nada más puede
  lanzarse hasta que alguno termine.
- Un programa en segundo plano no se puede detener desde el shell todavía;
  muere con el apagado. Detenerlo exige una capability de proceso
  transferible al shell (etapa posterior).
