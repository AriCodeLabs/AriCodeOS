# ADR 0007: Comportamiento de test por línea de comandos, no por features

- **Fecha:** 2026-09-26
- **Estado:** Aceptada

## Contexto

En M0 el kernel terminaba QEMU mediante una feature de Cargo (`qemu-exit`),
lo que obligaba a compilar binarios distintos para test y para uso normal y
dispersaba `#[cfg(feature)]` por el código.

## Decisión

AriBoot lee `\aricodeos\cmdline.txt` de la ESP y lo entrega al kernel en
`BootInfo` (protocolo v2). Claves reconocidas en la crate `aricode-cmdline`:
`qemu_exit` (terminar QEMU al acabar o fallar) y `test=<nombre>` (fallos
provocados). Un único binario de cada componente sirve para todos los
escenarios de `tests/qemu/*.toml`.

## Consecuencias

- Sin features de test ni `cfg` dispersos; el runner solo cambia un fichero.
- Los escenarios se declaran como datos y se validan con un test unitario.
- La cmdline queda disponible como mecanismo general de configuración de
  arranque para fases posteriores (nivel de log, modo seguro...).
