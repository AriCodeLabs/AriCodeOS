# Evidencias de la revisión de `4836023`

Las reproducciones se ejecutaron en copias temporales del commit, sin
modificar el kernel ni los servicios. Los fallos esperados de estas pruebas
son independientes de la batería oficial.

## Teclado

Copiar `keyboard-report.rs` a `libs/input/tests/review.rs` en una copia del
commit y ejecutar:

```sh
cargo test -p aricode-input --test review -- --nocapture
```

`keyboard.log` conserva la aserción fallida: dos pulsaciones nuevas en lugar
de seis; repetir el informe no entrega las omitidas.

## Intervalos

Copiar `interval.rs` a `libs/xhci/tests/review.rs` y ejecutar:

```sh
cargo test -p aricode-xhci --test review -- --nocapture
```

`interval.log` conserva las dos aserciones fallidas. Los valores esperados
proceden de la sección 6.2.3.6 de la especificación xHCI enlazada en el informe.

## Controlador EHCI adicional

En una copia sin los tests de host anteriores, aplicar
`legacy-controller.patch`, copiar `audit_usb_legacy.toml` a `tests/qemu` y
ejecutar:

```sh
cargo xtask test --all --area audit_usb_legacy
```

El parche solo añade `-device usb-ehci,id=review_ehci` cuando el escenario
activa `usb_hid`; conserva el xHCI y sus dispositivos. `legacy-serial.log`
recoge la salida del invitado. `legacy-tests.log` conserva el timeout de
90 segundos y la ausencia de los marcadores de arranque completo.

En la ejecución de auditoría se cambiaron 17007–17010 por 30007–30010 en
`sdk/proto/src/lib.rs`, `user/testprogs/src/bin/http_client.rs`,
`tests/qemu/shell_commands.toml` y `tests/qemu/network_stack.toml`.
También se usó un alias del ejecutable QEMU. Son ajustes de aislamiento;
no forman parte del defecto ni del parche de reproducción. No ejecutar
baterías simultáneas que compartan puertos.

## Batería oficial

`check.log` contiene el resultado correcto de `cargo xtask check`. El informe
principal recoge el resultado final de las pruebas oficiales; su copia
aislada usa puertos 29007–29010 y no contiene estas pruebas adicionales.

Resultado final: 212 pruebas de host y 81 escenarios QEMU correctos.
`tests-first.log` contiene 43 escenarios antes del agotamiento de `/tmp`;
`tests-remaining.log` contiene los 38 restantes y termina con cero fallos.
Los tests de host se repiten en el segundo lote; el recuento de 212 no los
duplica. `prior-regressions.log` conserva los marcadores de las correcciones
anteriores. Las imágenes y compilaciones temporales se eliminaron después
de guardar estas evidencias.
