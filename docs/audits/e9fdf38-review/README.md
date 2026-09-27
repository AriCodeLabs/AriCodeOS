# Reproducciones de la revisión de sandbox, Wasm, criptografía y A/B

Estas pruebas expresan el comportamiento esperado y fallan en la copia
revisada. No son correcciones ni forman parte de la batería normal.
`snapshot.json` identifica los archivos revisados y su commit base; el
arranque A/B estaba sin confirmar en Git al tomar la copia.

## Host

Desde la raíz del repositorio:

```bash
cargo test --manifest-path docs/audits/e9fdf38-review/Cargo.toml --bin wasm_probe -- --nocapture
cargo run --manifest-path docs/audits/e9fdf38-review/Cargo.toml --bin fs_probe
```

`wasm_probe` ejecuta directamente `aricode-wasm`: comprueba los límites de
memoria, el borrado al instanciar y crecer, y la función `start`.

`fs_probe` conserva la lógica de `Fds`, `qualified`, `ClientBuffers` y `handle`
extraída del servicio revisado. Usa AriFS y el protocolo originales, un disco
en memoria y tres consultas DMA simuladas. Demuestra que un prefijo distinto
puede usar el descriptor de la víctima; no simula IPC ni el kernel.

## QEMU: aislamiento y fugas del broker

Usar una copia independiente del repositorio para no añadir programas de
prueba a una imagen de trabajo:

1. Copiar `qemu/review_victim.rs` y `qemu/review_attacker.rs` a
   `user/testprogs/src/bin/`.
2. Crear `apps/review/` y copiar allí los tres archivos `.manifest`.
3. Copiar los dos `.toml` a `tests/qemu/`.
4. Ejecutar `cargo xtask test --all --area review`.

No es necesario crear paquetes: xtask descubre los binarios de testprogs y
los manifiestos. La víctima termina dejando abierto su archivo privado;
el atacante intenta leer descriptores 0–15 desde otro directorio. El segundo
escenario pide repetidamente `system, window` sin arrancar la GUI y después
intenta ejecutar `hello_app`.

## QEMU: arranque A/B

Los scripts `qemu/readonly_boot.py` y `qemu/early_confirm.py` se ejecutan
desde la raíz de la copia de pruebas, después de generar los escenarios
`boot.kernel_ready` y `crypto.random`. Necesitan los artefactos de Fase 30
en `build/esp` y `build/scenarios` y OVMF en las rutas indicadas.

Crean discos propios en `build/review-readonly` y `build/review-early-confirm`.
Copian la imagen firmada de A a B y preparan B con tres intentos. El primero
arranca dos veces en solo lectura, sin ejecutar init. El segundo permite
escrituras e introduce una red restringida para observar un fallo de init
posterior a la confirmación. Imprimen la serie relevante y el estado del disco;
no contienen aserciones automáticas de éxito o fracaso.

## Permisos de la clave privada

En un directorio temporal nuevo, ejecutar `xtask keygen` bajo `umask 022` y
examinar los permisos de `ed25519.seed` con `stat`. En la revisión se obtuvo
`0644`; no es necesario mostrar el contenido ni conservar la semilla.

## Aislamiento de la ejecución original

La primera batería de QEMU fue terminada con SIGTERM mientras otra batería
estaba activa en el workspace. La segunda usó una copia independiente, los
puertos 27007–27010 en lugar de 17007–17010 y nombres distintos para los
ejecutables del runner y QEMU. Solo se ajustaron las constantes de puertos de
prueba, el comando de QEMU y las expectativas correspondientes. Los archivos
de los hallazgos permanecieron iguales a `snapshot.json`.
