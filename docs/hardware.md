# AriCodeOS en hardware real (Fase 34)

Guía para el primer arranque en el portátil de pruebas. El orden y las
precauciones siguen las secciones 95 a 97 del documento maestro: **el disco
interno no se toca**; AriCodeOS arranca y escribe solo en su pendrive.

## Qué hace falta

- Un portátil x86-64 con firmware UEFI (arranque UEFI, no CSM/legacy) y
  Secure Boot desactivado. VT-d/"Intel Virtualization for Directed I/O" puede
  quedar activado: el kernel lo usa si la DMAR lo declara.
- Un pendrive de al menos 128 MiB que se pueda borrar por completo.
- Teclado: el interno (PS/2 por i8042) sirve de consola del kernel. Los
  teclados y ratones USB (HID de arranque) los atiende `usbd` sobre el xHCI
  y llegan al compositor con `gui`; el shell de la consola serie/PS/2 no los
  lee todavía.
- Sin puerto serie el diagnóstico es la pantalla: el kernel escribe la
  consola en el framebuffer GOP salvo con `gui`.

## Construir y grabar la imagen

```bash
cargo xtask usb-image                       # build/aricodeos-usb.img, cmdline "shell"
cargo xtask usb-image --cmdline "shell log=debug"
lsblk                                       # identificar el pendrive (p. ej. /dev/sdX), NUNCA el disco interno
sudo dd if=build/aricodeos-usb.img of=/dev/sdX bs=4M conv=fsync status=progress
```

La imagen es un disco GPT con seis particiones: `ESP` (FAT32 con AriBoot,
kernel, initrd y `cmdline.txt`), `ARI0` (AriFS con ficheros de ejemplo),
`SYSA` (imagen de sistema firmada, slot A activo), `SYSB` y `UPDATE` vacías
y `ARIBOOT` (estado A/B). Es la misma imagen que arranca el escenario
`usb.image_boot` en QEMU, así que lo que se graba ya ha arrancado una vez.

## Primer arranque

1. Con el portátil apagado, conectar el pendrive y entrar en el menú de
   arranque del firmware (F12, Esc, F9 según la marca). Elegir el pendrive
   en modo UEFI.
2. AriBoot muestra sus trazas en pantalla (slots, GOP, ACPI, kernel,
   cmdline, memoria, paginas, `ExitBootServices -> AriKernel`), pinta dos
   balizas en la esquina superior izquierda y salta al kernel; el kernel pinta
   la tercera, limpia la pantalla e informa de CPU, memoria, ACPI, APIC, SMP,
   PCI e IOMMU y lanza init.
3. Con `shell` en la cmdline aparece `ari> `. Comandos útiles: `cpus`,
   `mem`, `devices`, `ps`, `uptime`, `ls`, `cat`, `write`, `reboot`,
   `shutdown`. El sistema de ficheros es la partición ARI0 del propio
   pendrive, servida por usbd (ADR 0023).

## Estado en el portátil de pruebas (2026-10-10)

Primer arranque completo conseguido tras dos correcciones de AriBoot (GOP
sin exclusividad y tipos de memoria estándar, ADR 0021). El equipo: Intel de
2012 (xHCI `8086:1e31`, dos EHCI), Realtek `10ec:8136` (Ethernet) y
`10ec:5286` (lector de tarjetas), Atheros `168c:0032` (Wi-Fi), sin DMAR.

| Funciona | Falta |
|---|---|
| AriBoot: slots, GOP 800x600, kernel e initrd verificados | Por comprobar: el pendrive como servicio de bloques por USB (ADR 0023), fsd, confirmación del slot y shell por el teclado PS/2 |
| Kernel: memoria, ACPI, APIC, PCI (17 funciones), init y sus pruebas, `ARICODEOS_KERNEL_READY` | EHCI sin driver (solo xHCI) |
| Teclado PS/2 en la consola del kernel | Red: sin driver para la Realtek |
| xHCI con MSI (ADR 0022; el controlador no tiene MSI-X), 8 puertos, 16 scratchpads, servicio de entrada ok | Por comprobar: enrutado de los puertos al xHCI (en estos Intel nacen en los EHCI) y almacenamiento USB (ADR 0023) |

## Si algo falla

Los interruptores de la cmdline aislan subsistemas. Se editan en
`aricodeos/cmdline.txt` de la partición ESP del pendrive (montable en
cualquier sistema) o regenerando la imagen con `--cmdline`:

| Síntoma | Probar |
|---|---|
| Se para en "slots: arrancando desde el slot…" sin más líneas | Imagen anterior a 0.8.2: AriBoot abría el GOP en exclusiva y el firmware dejaba de pintar la consola. Regrabar con una imagen actual |
| Se para en "memoria: GetMemoryMap (tamano)" sin la línea de respuesta | El firmware no vuelve de `GetMemoryMap`. En imágenes anteriores a 0.8.2 lo provocaba el tipo de memoria propio de AriBoot (ADR 0021); con una imagen actual, anotar las líneas `kernel:` e `initrd.img:` |
| "GetMemoryMap: respuesta invalida" | El firmware responde fuera de la especificación; anotar la línea `memoria: GetMemoryMap ->` |
| Se para tras "ExitBootServices -> AriKernel" | Mirar las balizas (abajo) |
| Se cuelga tras "smp:" o con varias CPUs | `smp=off` |
| Pantalla negra o dispositivos que no responden tras "iommu:" | `iommu=off` |
| Demasiada salida | `log=warn`; poca: `log=debug` |
| No hay teclado | comprobar que el firmware expone el i8042 (algunos lo desactivan con "USB legacy"); la traza `console: teclado PS/2` debe aparecer |
| "usb-input: sin BAR 0 o sin IRQ entre las capabilities" | La función no tiene MSI ni MSI-X (la traza `pci: … reclamado (…)` lo dice); INTx no está soportado |
| "usb-input: fallo en la inicializacion del xHCI: …" | El paso que falló: capabilities, memoria DMA (el buffer se dimensiona con los scratchpads que pide el controlador), reset o arranque |
| "usb-input: sin almacenamiento USB" con el pendrive conectado | El pendrive no está en el xHCI: comprobar la traza `puertos Intel enrutados al xHCI`; si no aparece, el puerto cuelga de un EHCI sin driver (probar otro puerto, los azules suelen ser xHCI) |
| "usb-input: puerto N: …" con un error de INQUIRY, TEST UNIT READY o READ CAPACITY | El pendrive no responde al protocolo bulk-only; anotar la línea |
| "AriInit: sin particion ARI0 en el disco" | El pendrive no lleva la imagen de `cargo xtask usb-image` (o se grabó otra cosa encima) |
| "AriInit: fs_client salio con 6" u otro código, y antes "usb-input: WRITE(10) … rechazado: sentido k/asc/ascq" | El pendrive rechaza escrituras; el sentido (clave/asc/ascq) dice por qué (7/… = protegido contra escritura). Un pendrive que no admite `SYNCHRONIZE CACHE` se anuncia y no es un error |

Anotar (o fotografiar) las últimas líneas antes del fallo: `EXCEPTION`,
`KERNEL PANIC` y `iommu: fallo DMA` llevan la información necesaria.

### Balizas

Entre `ExitBootServices` y la primera línea del kernel no hay trazas (ya no
hay firmware y el kernel aún no tiene consola). Para ver hasta dónde llega el
arranque, AriBoot y el kernel pintan cuadrados de 32 px en la fila superior
de la pantalla (`Beacon` en `boot-protocol`): los tres primeros huecos son
blanco, verde y naranja; a su derecha, cuadrados cian por cada paso del
cargador desde la cmdline (sirven aunque la consola del firmware se pare).
La consola del kernel los borra al iniciarse, así que solo se ven si algo se
para antes.

| Pantalla | Dónde se paró |
|---|---|
| Última línea `cmdline:` o una de `memoria: GetMemoryMap…`, sin cuadrados cian | El firmware no responde: a su consola de texto (si falta la línea `memoria:`) o a `GetMemoryMap` (si está) |
| Cuadrados cian (hasta cuatro, a la derecha de los tres primeros huecos) y nada más | Paso del cargador en que se paró: 1 mapa de memoria leído, 2 tablas reservadas, 3 mapa físico construido, 4 kernel mapeado |
| Trazas de AriBoot hasta `ExitBootServices -> AriKernel`, ningún cuadrado | El framebuffer que anuncia la línea `GOP:` no es la pantalla (dirección o formato erróneos); anotar esa línea |
| Cuadrado blanco | En `ExitBootServices` (firmware) |
| Blanco y verde | En el salto al kernel: tablas de páginas, pila o la validación de `BootInfo` |
| Blanco, verde y naranja, sin texto | Entre la entrada del kernel y su consola (hash del initrd) o la consola no pinta |
| Texto del kernel | Seguir las trazas; ver la tabla anterior |

## Almacenamiento

El primer arranque no necesita escribir nada. Cuando se pruebe el servicio
de bloques sobre el pendrive (AHCI/NVMe/USB no tienen driver todavía; el
pendrive solo lo lee el firmware para arrancar), se hará primero en modo
solo lectura y siempre sobre el propio pendrive o un USB dedicado.
