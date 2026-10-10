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
   `mem`, `devices`, `ps`, `uptime`, `reboot`, `shutdown`.

## Si algo falla

Los interruptores de la cmdline aislan subsistemas. Se editan en
`aricodeos/cmdline.txt` de la partición ESP del pendrive (montable en
cualquier sistema) o regenerando la imagen con `--cmdline`:

| Síntoma | Probar |
|---|---|
| Se para en "slots: arrancando desde el slot…" sin más líneas | Imagen anterior a 0.8.2: AriBoot abría el GOP en exclusiva y el firmware dejaba de pintar la consola. Regrabar con una imagen actual |
| Se para tras "ExitBootServices -> AriKernel" | Mirar las balizas (abajo) |
| Se cuelga tras "smp:" o con varias CPUs | `smp=off` |
| Pantalla negra o dispositivos que no responden tras "iommu:" | `iommu=off` |
| Demasiada salida | `log=warn`; poca: `log=debug` |
| No hay teclado | comprobar que el firmware expone el i8042 (algunos lo desactivan con "USB legacy"); la traza `console: teclado PS/2` debe aparecer |

Anotar (o fotografiar) las últimas líneas antes del fallo: `EXCEPTION`,
`KERNEL PANIC` y `iommu: fallo DMA` llevan la información necesaria.

### Balizas

Entre `ExitBootServices` y la primera línea del kernel no hay trazas (ya no
hay firmware y el kernel aún no tiene consola). Para ver hasta dónde llega el
arranque, AriBoot y el kernel pintan cuadrados de 32 px en la fila superior
de la pantalla (`Beacon` en `boot-protocol`); la consola del kernel los borra
al iniciarse, así que solo se ven si algo se para antes.

| Pantalla | Dónde se paró |
|---|---|
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
