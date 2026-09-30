# ADR 0020: Recepción desde varios endpoints (`ipc_receive_any`)

- **Fecha:** 2026-09-29
- **Estado:** Aceptada

## Contexto

fsd atiende un endpoint principal y hasta ocho concedidos (directorios
privados y ficheros compartidos), pero `ipc_receive` solo espera en uno:
el bucle esperaba 10 ms en el principal y sondeaba el resto sin espera.
Una petición por un endpoint concedido (todas las aplicaciones normales)
esperaba de media 5 ms a que venciera esa espera. `bench` (etapa 5) lo
midió: la llamada IPC a fsd desde una aplicación costaba milisegundos
cuando el resto del camino cuesta microsegundos. Es la primera
optimización guiada por medidas.

## Decisión

1. **Syscall `ipc_receive_any` (31)**: recibe el mensaje más antiguo del
   primero (por orden en la lista) de hasta 16 endpoints que tenga alguno,
   con el mismo plazo y los mismos buffers que `ipc_receive`; devuelve
   además el índice del endpoint. Exige `READ` sobre todos y que todos sean
   endpoints, comprobado antes de esperar.
2. **Una cola de espera global para los receptores múltiples**
   (`ANY_RECEIVERS`): cada envío despierta al receptor propio del endpoint
   (`wake_one`) y a todos los receptores múltiples (`wake_all`), que
   vuelven a comprobar sus endpoints. Los despertares de más son el precio
   de no registrar un hilo en varias colas; con pocos servicios es
   despreciable y se puede cambiar sin tocar la interfaz.
3. **fsd espera en todos sus endpoints a la vez** (`ipc_receive_any` con
   plazo de un segundo) y despacha por índice; los servicios que combinan
   IRQ e IPC (drivers de entrada) siguen con su bucle.

## Alternativas descartadas

- Un endpoint por servicio con el espacio de nombres dentro del mensaje:
  rompe el modelo de capabilities (el endpoint es la autoridad).
- Registrar el hilo en la cola de cada endpoint: correcto pero exige
  cancelar registros al despertar y complica `WaitQueue`; se deja para
  cuando las medidas lo pidan.
- Hilos por endpoint en fsd: no hay hilos de usuario y multiplicaría el
  estado compartido.

## Consecuencias

- `ipc.receive_any` (programa `ipc_any`) cubre plazo, índice, varios
  mensajes, handle sin `READ` y handle que no es endpoint.
- `docs/performance.md` recoge la latencia de `ipc fsd list` antes y
  después: de milisegundos (dominada por la espera de 10 ms) a decenas de
  microsegundos bajo TCG.
