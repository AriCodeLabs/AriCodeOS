# Medidas de rendimiento

Qué se mide, cómo se reproduce y qué significan los números. Todas las
cifras son de QEMU con TCG (sin KVM) en el equipo de desarrollo: sirven para
comparar builds entre sí, no para predecir hardware real. Un cambio de host,
de modo de QEMU o de número de CPUs invalida la comparación.

## Cómo

```bash
cargo xtask run --cmdline "shell"
ari> run bench
ari> cpus
```

O el escenario `perf.bench` (`cargo xtask test --all --area perf`), cuyo
`serial.log` conserva los números. Ejecutar `run bench` dos veces y quedarse
con la segunda: la primera paga las páginas del programa y los buffers.

## Qué mide `bench`

`apps/bench` es una aplicación normal del broker (`system`, `fs`, 64 KiB):
no ve nada que no vea cualquier otra aplicación.

| Medida | Qué incluye | Muestras |
|---|---|---|
| `syscall thread_get_tid` | entrada y salida por `syscall`, sin trabajo | 10 000 |
| `ipc fsd list` | `rpc::call` completo: crear endpoint de respuesta, enviar, esperar, recibir, cerrar; el bucle de fsd (espera de 10 ms en el principal, sin espera en los concedidos) | 1 000 |
| `replace 4 KiB` | reemplazo atómico: journal, dos `flush` a virtio-blk, copia y limpieza | 100 |
| `read 4 KiB` | una lectura por IPC con copia por el buffer DMA del cliente | 100 |

Percentiles p50, p95 y p99 sobre el TSC, calibrado contra el reloj del
sistema (200 ms; granularidad de 10 ms, error menor del 5 %).

## Reposo por CPU

`cpus` muestra, además de las CPUs en línea, el porcentaje de ciclos que
cada una ha pasado en `hlt` desde el arranque (`sysinfo::IDLE`): con el
sistema quieto debe ser alto; una CPU al 0 % en reposo tiene un hilo que
no duerme (`ps` lo enseña). Es la base de las medidas de energía: sin
`hlt` no hay estados de reposo que ahorren nada.

## Informe

`diag` reúne en un solo bloque `uptime`, `mem`, `cpus`, `ps` y
`services --detail`: es lo que se pide a alguien que informa de un problema.

## Referencia (0.7, QEMU 10 TCG, 1 CPU, `-cpu qemu64`)

Se rellena con la primera ejecución de `perf.bench` de cada versión; los
valores sirven de alarma (una regresión del doble se nota), no de garantía.

Calibración: 3 813 066 ciclos de TSC por ms.

| Medida | p50 | p95 | p99 | Nota |
|---|---|---|---|---|
| syscall (ns) | 99 | 109 | 119 | TCG traduce el camino una vez; no extrapolar a hardware |
| ipc fsd list (µs) | 9 993 | 10 078 | 10 227 | dominada por la espera de 10 ms del bucle de fsd sobre el endpoint principal: las peticiones por endpoints concedidos esperaban a que venciera (ADR 0020 lo corrige) |
| replace 4 KiB (µs) | 9 999 | 10 266 | 12 585 | ídem; el trabajo real (journal y dos `flush`) queda enmascarado |
| read 4 KiB (µs) | 9 994 | 10 101 | 10 135 | ídem |

Reposo de la CPU tras el arranque con shell: 81-82 %.

### Tras `ipc_receive_any` (ADR 0020), misma build salvo fsd

| Medida | p50 | p95 | p99 | Nota |
|---|---|---|---|---|
| syscall (ns) | 99 | 129 | 129 | sin cambios |
| ipc fsd list (µs) | 44 | 46 | 58 | 227 veces menos: fsd espera en todos sus endpoints a la vez |
| replace 4 KiB (µs) | 5 708 | 6 343 | 6 458 | ahora se ve el trabajo real: unas 20 peticiones de un sector al servicio de bloques y cuatro `flush`; el siguiente candidato es agrupar sectores por petición |
| read 4 KiB (µs) | 712 | 800 | 838 | ocho lecturas de un sector por IPC al servicio de bloques |

### Tras los lotes de sectores y un `flush` menos (etapa 5), misma build salvo AriFS y fsd

| Medida | p50 | p95 | p99 | Nota |
|---|---|---|---|---|
| syscall (ns) | 99 | 109 | 129 | sin cambios |
| ipc fsd list (µs) | 46 | 49 | 63 | sin cambios |
| replace 4 KiB (µs) | 2 628 | 2 817 | 2 997 | tres `flush` (cada uno un fsync del host, de 0,5 a 3 ms según el disco) y cinco peticiones al servicio de bloques; entre ejecuciones varía con el host más que con el código |
| read 4 KiB (µs) | 141 | 150 | 234 | una petición de ocho sectores en vez de ocho de uno |

Lo que queda del coste de `replace` es el fsync: reducirlo exige menos
compromisos (agrupar reemplazos) o un soporte con caché de escritura con
batería, no más código.
