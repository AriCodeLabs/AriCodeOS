# Evidencias de la revisión de `3eb4965`

Estas pruebas son instrumentación de auditoría, no correcciones. Se
ejecutaron en copias temporales; no modifican el código de producción.

## Recogida de procesos

- `reap-interleaving.patch`: barrera en `release_address_space` y
  observación de la retirada en `wait_until`. Sustituye la cesión por espera
  activa en esa ventana para que la CPU del padre no recoja el proceso.
- `audit_reap.toml`: adaptación de `user.hello` con cuatro CPU.
- `reap-serial.log`: resultado observado, `mapper=true` al retirar el
  proceso y `process_present=false` cuando continúa la recogida.

En una copia de `3eb4965`, aplicar el parche, copiar el TOML a `tests/qemu`
y ejecutar `cargo xtask test --all --area audit_reap`. El escenario conserva
las expectativas funcionales de `user.hello` y pasa; los marcadores `AUDIT`
documentan la intercalación defectuosa.

## Reasignación de IOMMU

- `iommu-reassignment.patch`: añade una comprobación de ciclo de vida al
  principio de `iommu::selftest`, usando dos identificadores de dominio de
  prueba y la función PCI `00:02.0` de la configuración QEMU del proyecto.
- `audit_iommu.toml`: selección independiente de ese escenario.
- `iommu-serial.log`: contexto presente en el segundo dominio antes de
  liberar el primero, ausente después; aserción fallida.

En otra copia limpia de `3eb4965`, aplicar solo este parche, copiar el TOML
a `tests/qemu` y ejecutar `cargo xtask test --all --area audit_iommu`.
En el commit revisado se espera un fallo de aserción.

Para ambas reproducciones se usaron puertos 28007–28010 en lugar de
17007–17010 y un alias del ejecutable QEMU. La batería oficial aislada usó
27007–27010. Son ajustes para evitar interferencias con otras ejecuciones;
no forman parte de los parches del kernel. No ejecutar dos baterías en los
mismos puertos simultáneamente.

## Batería oficial

`check.log`: formato y Clippy correctos. `tests.log`: 203 pruebas de host
y 77 escenarios QEMU correctos, sobre la copia aislada sin instrumentación
del kernel. Los fallos de las reproducciones adicionales no pertenecen a
esta batería oficial.
