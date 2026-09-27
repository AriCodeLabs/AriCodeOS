# Claves de desarrollo

Par Ed25519 **solo para desarrollo y tests en QEMU**. La semilla
(`ed25519.seed`) está en el repositorio a propósito: cualquiera puede firmar
imágenes que AriBoot aceptará en el entorno de pruebas. Un sistema real usa un
par generado fuera del repositorio (`cargo xtask keygen DIR`) cuya semilla no
se versiona nunca; solo la clave pública se incorpora al cargador.

- `ed25519.seed`: semilla privada (32 bytes, hex). No usar fuera de pruebas.
- `ed25519.pub`: clave pública (32 bytes, hex).
