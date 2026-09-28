# ADR 0012: Plazos, correlación y reintentos en las llamadas IPC del SDK

- **Fecha:** 2026-09-28
- **Estado:** Aceptada

## Contexto

Los clientes del SDK (bloques, ficheros, red, ventanas, entrada, broker)
reutilizaban un endpoint de respuesta por cliente y esperaban sin plazo. Una
respuesta que llegara después de un plazo vencido quedaba en ese endpoint y
la consumía la petición siguiente; un servidor muerto dejaba al cliente
esperando para siempre. El estudio de 2026-09-28 (etapa 1B) pide un contrato
uniforme de fallos, plazos y reintentos.

## Decisión

1. **Un endpoint de respuesta por petición** (`aricode_sys::rpc::call`): el
   cliente crea el endpoint, envía su duplicado con la petición, espera la
   respuesta y lo cierra. Una respuesta tardía llega a un endpoint cerrado
   y se pierde; nunca se atribuye a otra petición. No hace falta cambiar
   los protocolos: la correlación la da el canal, no un identificador.
2. **Toda espera está acotada**: envío y recepción llevan plazo. Cada
   cliente elige el suyo (plazo pedido más un margen para el servicio); solo
   el broker de lanzamiento espera sin plazo la salida del programa, porque
   esa espera es el resultado.
3. **Un plazo vencido no deshace nada**: el resultado de esa petición es
   desconocido (pudo ejecutarse). El SDK no reintenta escrituras por su
   cuenta; el llamador decide con esa información. Las lecturas y consultas
   sin efectos pueden reintentarse.
4. **Servidor muerto**: el cliente recibe `Timeout`; con el supervisor
   (ADR 0011) el endpoint sigue siendo válido y la siguiente generación
   atiende las peticiones nuevas.

## Consecuencias

- Coste de dos syscalls más por llamada (crear y cerrar el endpoint), que
  las medidas de IPC (`ipc.ping_pong`) siguen acotando.
- `test=ipc_late` (`ipc.late_reply`) demuestra el descarte de la respuesta
  tardía y la correlación; los escenarios de servicios cubren los plazos
  con servidores muertos.
