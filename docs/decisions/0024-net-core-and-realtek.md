# ADR 0024: Núcleo de red compartido y driver Realtek r8169

- **Fecha:** 2026-10-10
- **Estado:** Aceptada

## Contexto

El portátil de pruebas (Fase 34) tiene una Realtek `10ec:8136` (familia
r8169, 10/100 PCIe) y un Wi-Fi Atheros AR9485. Toda la pila de red (ARP,
IPv4, ICMP, UDP, DHCP, DNS, TCP) y el servidor de `NetServiceProtocol v1`
vivían dentro de `services/virtio-net`, pegados a las colas virtio, así que
un segundo driver habría duplicado 900 líneas. QEMU no emula ninguna
Realtek de esta familia (solo la RTL8139, con otro modelo de programación),
y un driver Wi-Fi con WPA2 es una obra de meses: la red por cable es la vía
realista para que el portátil use la pila que ya existe.

## Decisión

1. **`ari-net-core`**: la pila y el servidor salen de virtio-net a una
   crate de usuario compartida, generica sobre el trait `Link` (`mac`,
   `send_frame`, `recv_frame`) con `run(link, clock, name)`. virtio-net
   queda en su capa de enlace. Sin cambios de protocolo ni de
   comportamiento (`network.*` siguen igual).
2. **`aricode-rtl8169`** (librería de anfitrión): registros, bits,
   descriptores de 16 bytes y la secuencia de arranque de la familia
   (`reset`, `start`, `ack_interrupts`, `link_state`), sobre un trait
   `Regs` que el driver implementa con MMIO y los tests con memoria. Es la
   única parte que se puede probar sin la tarjeta y se prueba ahí.
3. **`services/rtl-net`** (`rtl_net`): primer BAR de memoria, un buffer DMA
   con 16 descriptores de recepción y 8 de transmisión (2 KiB por ranura),
   tramas rellenas a 60 bytes, enlace esperado hasta 3 s y después `run`.
   Interrupciones por MSI/MSI-X (ADR 0022).
4. **init**: sin virtio-net, la primera función Ethernet (clase 02.00) de
   Realtek lanza `rtl_net` sobre el mismo endpoint de red. Los clientes de
   prueba (`net_client`, `tcp_client`, `http_client`) no se ejecutan porque
   hablan con el respondedor de QEMU; en el portátil la prueba son `net`,
   `ping`, `dns` y `http` del shell.
5. **Sin escenario QEMU para rtl-net**: no hay emulación. La validación es
   la librería en el anfitrión, los escenarios de red sobre el núcleo
   compartido y la foto del portátil (`docs/hardware.md`).

## Consecuencias

- Un tercer driver (por ejemplo e1000e para otros equipos) solo tiene que
  implementar `Link`.
- rtl-net no programa el PHY: cuenta con que el firmware dejó la
  autonegociación hecha. Si el enlace no sube, lo anuncia y DHCP reintenta.
- Sin suma de comprobación por hardware ni VLAN; una trama por descriptor.
- El Wi-Fi queda fuera: requeriría ath9k, 802.11 y WPA2.
