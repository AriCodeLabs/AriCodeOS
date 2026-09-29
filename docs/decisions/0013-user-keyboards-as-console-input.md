# ADR 0013: Teclados de usuario como entrada de consola (puente de consola)

- **Fecha:** 2026-09-28
- **Estado:** Aceptada

## Contexto

La consola es del kernel: COM1 y el teclado PS/2 llegan por interrupción a
un anillo que `console_read` (syscall 24) entrega a quien tiene `READ`
sobre el objeto `Console`. Los teclados virtio-input y USB HID viven en
ring 3 (`inputd`, `usbd`) y solo los leen el compositor y los programas
que reciben su endpoint. En un equipo sin puerto serie ni PS/2 (un portátil
con teclado USB interno, Fase 34) el shell no tenía teclado. La etapa 2 del
plan posterior al roadmap pide una sesión útil con esos teclados.

## Decisión

1. **La consola sigue en el kernel.** El shell lee un solo anillo sin saber
   de dónde viene cada byte; COM1 y PS/2 siguen funcionando sin procesos de
   usuario (pánicos, arranque temprano, tests de consola).
2. **Nueva syscall `console_inject` (29)**: escribe texto en el anillo de
   entrada de consola. Exige `MANAGE` sobre un objeto `Console`; `READ`
   (lo que reciben el shell y los programas) no basta. Solo init recibe el
   handle con `MANAGE` y lo delega a quien decide.
3. **Un servicio `console_bridge`** (`services/console-bridge`) recibe la
   entrada de consola con `MANAGE` y los endpoints de los servicios de
   entrada, traduce las pulsaciones con `aricode_input::Keyboard` (un
   estado por teclado, los botones del ratón se ignoran) y las inyecta.
   Init lo lanza como residente supervisado (ADR 0011, política de driver)
   en modo `shell`, solo cuando no hay compositor: los servicios de entrada
   admiten un lector a la espera y con GUI los eventos son del compositor.
4. **Un lector por servicio de entrada.** Mientras el puente corre, un
   programa lanzado desde el shell que pida entrada compite con él y verá
   `Busy` o perderá pulsaciones. Es el contrato de la etapa 2 (sesión de
   shell); la sesión gráfica no lanza el puente.

## Alternativas descartadas

- Mover la consola a espacio de usuario (un `consoled` con COM1 y PS/2):
  añade un proceso al camino de pánico y arranque temprano y obliga a
  transferir la línea serie, sin ganar nada para el shell.
- Que el shell lea los endpoints de entrada él mismo: duplica la traducción
  en cada lector de consola (shell, programas de prueba) y ata al shell a
  los servicios de entrada; con el puente el shell sigue leyendo un
  `Console` con `READ`.
- Un sistema de foco de entrada (rutar eventos entre puente, compositor y
  aplicaciones): es la etapa 4 (aplicaciones externas), no la 2.

## Consecuencias

- Init recibe `0x10008` con `READ|MANAGE|DUPLICATE|TRANSFER` y sigue
  pasando al shell solo `READ|TRANSFER`.
- El puente recorre los servicios por turnos con plazo `POLL_MS` (20 ms):
  la latencia máxima de una pulsación crece con el número de dispositivos.
- El puente no escribe nada al arrancar: init anuncia su lanzamiento
  (`CONSOLE_BRIDGE_READY`) antes de crear el shell, porque con varias CPUs
  una línea del puente se entrelazaba con el primer prompt.
- `shell.usb_keyboard` y `shell.virtio_keyboard` prueban una sesión de
  shell tecleada por QMP en un teclado USB HID y en uno virtio;
  `shell.services_detail` muestra el puente como residente.
