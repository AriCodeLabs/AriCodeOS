# ADR 0022: MSI además de MSI-X para los drivers de usuario

- **Fecha:** 2026-10-10
- **Estado:** Aceptada

## Contexto

`pci_claim` entregaba a los drivers una IRQ solo si la función tenía MSI-X
(entrada 0 programada por el kernel). En QEMU todos los dispositivos que
usamos (virtio, qemu-xhci) tienen MSI-X, así que nunca faltó. En el primer
arranque físico (Fase 34) el xHCI del portátil, un Intel 7 series
(`8086:1e31`), solo ofrece MSI: el kernel lo reclamaba sin IRQ, init no
ponía el bit 15 de la máscara y `usb-input` salía con "fallo en la
inicializacion del xHCI" (código 10) sin más explicación. Lo mismo le pasará
a AHCI, NVMe antiguos y casi cualquier controlador de 2012.

## Decisión

1. **MSI como segunda opción.** `pci_claim` programa MSI-X si la función la
   tiene; si no, la capability MSI con un solo mensaje (dirección hacia el
   LAPIC de arranque, datos = vector, máscara por vector desenmascarada si
   existe). El driver no distingue el origen: recibe el mismo handle IRQ y
   usa `irq_wait`. INTx sigue sin soportarse.
2. **`IrqSource::Msi { cfg_virt, cap }`** en `hw`: enmascarar es limpiar el
   bit de habilitación en el control de la capability (ECAM, sin caché). La
   asignación de vector y la creación de la línea se comparten con MSI-X.
3. **La traza de reclamación dice el origen**: `reclamado (MSI-X entrada 0)`,
   `reclamado (MSI)` o `reclamado (sin MSI ni MSI-X: sin IRQ)`.
4. **`usb-input` explica el fallo de capabilities**: "sin BAR 0 o sin IRQ
   entre las capabilities" frente a "no se pudo mapear el BAR 0".
5. **Escenario `shell.usb_keyboard_msi`**: `usb_hid_msi = true` arranca el
   xHCI de QEMU con `msi=on,msix=off`; se exige `reclamado (MSI)` y que el
   teclado USB llegue al shell.

## Consecuencias

- Un solo vector por función: MSI con varios mensajes (MME > 0) no se usa;
  los drivers de hoy esperan una IRQ.
- Funciones sin MSI ni MSI-X se reclaman sin IRQ (como antes) y el driver lo
  ve en la máscara; INTx por I/O APIC queda para cuando un dispositivo lo
  necesite.
