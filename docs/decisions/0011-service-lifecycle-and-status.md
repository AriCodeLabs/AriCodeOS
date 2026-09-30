# ADR 0011: Ciclo de vida de los servicios residentes y estado consultable

- **Fecha:** 2026-09-28
- **Estado:** Aceptada

## Contexto

Hasta 0.6.0, `init` lanzaba cada driver y servicio residente una vez y solo
comprobaba su salida en el arranque. Un servicio que moría después dejaba a
sus clientes esperando en un endpoint sin servidor, y nada informaba de ello.
El estudio `docs/audits/2026-09-28-referentes-capacidades-oportunidades.md`
(etapa 1, "recuperación observable") pide un contrato común de vida.

## Decisión

1. **Estados y generaciones** (`libs/supervisor`, sin dependencias, probado en
   host): `Stopped`, `Starting`, `Ready`, `Degraded`, `Failed`. Cada arranque
   de un servicio recibe una generación nueva. Un servicio es obligatorio u
   opcional: si un opcional agota su presupuesto, el sistema queda
   degradado; si lo agota uno obligatorio, fallido.
2. **Reinicio con presupuesto**: tras una muerte (salida, fallo o plazo de
   arranque vencido) el supervisor espera `backoff_initial · 2^(n-1)` ms,
   acotado a `backoff_max`, hasta `max_restarts` veces; tras `stable_ms` en
   `Ready` el presupuesto se restaura. Política por defecto para drivers:
   5 reinicios, 100 ms a 5 s, plazo de arranque 5 s, estabilidad 60 s.
3. **El endpoint es de init y no cambia**: la identidad estable de un servicio
   es su endpoint; cada generación recibe un duplicado. Los clientes siguen
   enviando al mismo endpoint y la nueva generación atiende lo encolado.
   Una función PCI cuyo driver murió puede reclamarse de nuevo: el kernel la
   considera libre cuando ya no existe ningún handle de su objeto de
   configuración (`PciDevice::claimed` es un `Weak`).
4. **Publicar solo lo que responde**: un servicio con sonda (`Info` de
   entrada, `ping`) se publica cuando contesta; si no contesta o su proceso
   termina, cuenta como muerte por plazo de arranque.
5. **Estado consultable**: protocolo `status` v1 (`sdk/proto`, protocolo 7)
   servido por init en el endpoint del broker, una petición por servicio;
   `services --detail` en el shell lo muestra. No lleva contenido privado.
6. **Test por cmdline** (ADR 0007): `restart_storm` somete a un servicio de
   prueba a 100 reinicios mientras fsd sigue atendiendo y comprueba que la
   memoria libre vuelve al nivel anterior.

## Nota (auditoría 6ee19fb, hallazgo 4)

Cuando la sonda de arranque vence, el supervisor no da por muerta la
generación sin más: si el proceso sigue vivo lo termina con `process_kill`
y espera a que el kernel lo recoja (hasta 2 s) antes de anunciar la muerte
y programar la siguiente; así la nueva generación no compite con la
anterior por sus dispositivos PCI. Si aun así no termina, el servicio queda
fallido sin relanzar (`services.start_timeout`).

## Consecuencias

- La supervisión vive en init mientras init viva (modo `shell`/`gui`); en
  un arranque de prueba sin shell init termina y no hay a quién reiniciar.
- Un cliente con una petición en curso cuando el servidor muere no recibe
  respuesta: el resultado de esa petición es desconocido. Los clientes
  acotan sus esperas y no reintentan escrituras automáticamente (etapa 1B
  se ocupará de la correlación y la semántica de reintento).
- Reiniciar un driver no detiene el DMA en curso del dispositivo: con el
  IOMMU activo el dominio del proceso muerto se retira antes de que sus
  frames se reutilicen; sin IOMMU el reinicio es un riesgo documentado.
