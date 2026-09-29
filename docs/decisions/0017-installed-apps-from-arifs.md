# ADR 0017: Aplicaciones fuera del árbol, empaquetadas e instaladas en AriFS

- **Fecha:** 2026-09-29
- **Estado:** Aceptada

## Contexto

Hasta ahora todo programa vivía en el initrd: añadir una aplicación exigía
tocar el workspace y regenerar la imagen. La etapa 4 del plan (estudio de
2026-09-28) pide que una aplicación desarrollada fuera del árbol se
construya, se instale, se ejecute y se actualice, con identidad por editor
y contenido, manifiesto firmado e instalación transaccional.
`process_spawn` solo aceptaba nombres del initrd.

## Decisión

1. **Paquete = un fichero firmado** (`aricode-pkg`): contenedor
   `aricode-initrd` con `manifest` (nombre, versión, SHA-256 del programa
   y de su manifiesto de aplicación), `manifest.sig` (Ed25519 sobre el
   manifiesto, la clave de desarrollo como en las imágenes de sistema),
   `program` (ELF) y `program.manifest`. Instalado es `apps/NOMBRE.aripkg`
   en el espacio principal de fsd. El manifiesto de aplicación viaja dentro
   del paquete y por tanto está firmado: un programa instalado no puede
   pedir más que uno del initrd con el mismo manifiesto.
2. **Un solo fichero, instalación atómica**: instalar o actualizar es un
   `replace` de AriFS (ADR 0014); tras un corte hay la versión anterior o
   la nueva, nunca una mezcla de programa y manifiesto.
3. **El broker verifica al lanzar.** Si el programa no está en el initrd,
   lee `apps/NOMBRE.aripkg` a un buffer DMA de preparación (256 KiB,
   reservado una vez), verifica firma y hashes, aplica la misma política al
   manifiesto de aplicación y crea el proceso con `process_spawn_from`
   directamente desde el buffer (desplazamiento y longitud del ELF). Un
   paquete que no verifica no se lanza (`Denied`, con el motivo en la
   consola).
4. **Nueva syscall `process_spawn_from` (30)**: como `process_spawn`, con el
   ELF en un buffer DMA del llamador (`spawn_from::Request`: handle,
   desplazamiento, longitud, nombre). Exige `MANAGE` sobre el `SPAWNER` y
   `READ` sobre el buffer; el kernel copia los segmentos al crear el proceso
   y el buffer puede reutilizarse después. El ELF se analiza con
   comprobación de límites sobre memoria que el llamador podría estar
   modificando: eso solo puede producir un proceso inválido del propio
   llamador, nunca un fallo del kernel.
5. **Construcción fuera del árbol**: `cargo xtask app-pack DIR [--version N]`
   compila `DIR/Cargo.toml` (su propio workspace, SDK por ruta) desde la
   raíz del repositorio, así que `rust-toolchain.toml` y `.cargo/config.toml`
   se aplican igual que a los programas del initrd, fija `APP_VERSION` al
   compilar, firma y deja `build/pkgs/NOMBRE-N.aripkg`. Un escenario declara
   `install = ["DIR"]` y xtask lo instala en el disco de pruebas (AriFS
   admite ficheros de capacidad múltiple de 64 sectores: `create_sized`).

## Alternativas descartadas

- ELF y manifiesto sueltos en AriFS (`apps/NOMBRE` + `apps/NOMBRE.manifest`):
  sin firma y con dos ficheros que actualizar, un corte podía dejar programa
  nuevo con manifiesto viejo.
- Verificar solo al instalar: quien escriba en AriFS podría sustituir el
  programa después; verificar al lanzar hace que la autoridad dependa de la
  clave, no del acceso al disco.
- Mapear el ELF instalado directamente desde el disco o pasarlo por
  `copy_from_user`: AriFS no ofrece páginas, y la copia grande ata el tamaño
  al heap de init; el buffer DMA ya es memoria contigua accesible por el
  mapa físico.

## Consecuencias

- `apps.external`: `hello_ext` se construye desde `examples/hello_ext`, se
  empaqueta con versión 1, se instala en el disco de pruebas y `run
  hello_ext` lo lanza dos veces con su manifiesto.
- El paquete instalado está acotado por el journal de AriFS (32 KiB) si se
  quiere actualizar de forma atómica, y por el buffer de preparación (256
  KiB) para lanzarse; los programas del SDK ocupan decenas de KiB.
- La instalación y actualización desde dentro del sistema (un instalador
  de confianza, paquetes de más de una versión, rechazo de firmas malas y
  cortes durante la instalación) es la siguiente parte de la etapa.
