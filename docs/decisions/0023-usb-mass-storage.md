# ADR 0023: Almacenamiento USB como servicio de bloques dentro de usbd

- **Fecha:** 2026-10-10
- **Estado:** Aceptada

## Contexto

En el portátil de pruebas (Fase 34) el único disco que AriCodeOS puede usar
es su propio pendrive, y el único driver de bloques era virtio-blk. Sin
servicio de bloques no hay fsd, no se confirma el slot y init no lanza el
shell: el sexto arranque físico terminaba en `ARICODEOS_KERNEL_READY` sin
nada que hacer. Además, en los Intel 7 series los puertos USB nacen
enrutados a los EHCI (sin driver) y el xHCI no veía ningún dispositivo.

## Decisión

1. **El driver xHCI (`usb-input`, "usbd") atiende también el almacenamiento
   masivo.** Un controlador tiene un solo dueño, así que la clase de
   almacenamiento vive en el mismo proceso que HID: enumera la primera
   interfaz bulk-only (clase 8, subclase 6, protocolo 0x50), configura sus
   endpoints masivos, identifica el dispositivo (`INQUIRY`), espera a que
   esté listo (`TEST UNIT READY` con `REQUEST SENSE`) y lee la capacidad
   (`READ CAPACITY(10)`; solo bloques de 512 bytes). Un dispositivo como
   máximo.
2. **Transporte y SCSI en una librería de anfitrión** (`aricode-usb-storage`:
   CBW/CSW y los CDB `INQUIRY`, `TEST UNIT READY`, `REQUEST SENSE`, `READ
   CAPACITY(10)`, `READ(10)`, `WRITE(10)`, `SYNCHRONIZE CACHE(10)`), con
   tests. `aricode-xhci` añade endpoints masivos, TDs que no cruzan 64 KiB
   (`bulk_trbs`) y los comandos de recuperación (Reset Endpoint, Set TR
   Dequeue, `CLEAR_FEATURE(HALT)`) que BOT exige tras un STALL.
3. **Segundo extra del contrato de driver: el endpoint de bloques.** init
   pasa a cada xHCI, tras `SYSTEM`, un endpoint que usbd sirve con
   `aricode-proto::block` (`Info`, `Read`, `Write`, `Flush` =
   `SYNCHRONIZE CACHE`) junto a la entrada, con `ipc_receive_any`. Sin
   dispositivo, `Info` responde `Invalid`. El extra es opcional: el driver
   comprueba que el handle sea un endpoint (el banco de pruebas del kernel
   no lo pasa).
4. **Sin virtio-blk, init usa el bloque USB.** Tras lanzar los xHCI, si no
   hay servicio de bloques pregunta la capacidad al primero; si responde,
   es el servicio de bloques y sobre él va la misma pila (fsd en ARI0,
   clientes de prueba, confirmación del slot) que con virtio-blk
   (`start_storage_stack`).
5. **Cesión del firmware y enrutado Intel.** Antes de tocar el controlador
   usbd pide la propiedad por la capability USB Legacy Support y apaga sus
   SMI; en los xHCI Intel escribe `XUSB2PR` y `USB3_PSSEN` con las máscaras
   `USB2PRM`/`USB3PRM` (como Linux) para que los puertos compartidos pasen
   de los EHCI al xHCI, y espera más antes de enumerar.
6. **Escenario `usb.storage`** (`usb_storage = true`): el disco de pruebas
   cuelga del xHCI de QEMU como `usb-storage` y no hay virtio-blk; fsd y el
   shell van sobre usbd y un fichero escrito sobrevive a un reinicio.

## Consecuencias

- Un pendrive conectado al xHCI es el disco del sistema en el portátil: el
  mismo pendrive del que arranca AriBoot. Escrituras solo ahí (sección 95).
- Las peticiones de bloques van en serie con la entrada HID en el mismo
  bucle; un teclado USB sigue respondiendo porque cada petición de bloques
  dura milisegundos.
- `READ(10)`/`WRITE(10)` limitan el disco a 2 TiB; los sectores de 4 KiB
  nativos no se admiten.
- Séptima foto del portátil: el pendrive se lee pero `fs_client` falla al
  crear un fichero. Muchos pendrives rechazan `SYNCHRONIZE CACHE` como
  petición ilegal (no tienen caché que vaciar): usbd lo detecta por el
  sentido (`Sense::command_unsupported`), lo anuncia una vez y el flush
  pasa a ser inmediato; los demás comandos rechazados se anuncian con su
  sentido para diagnosticarlos desde una foto.
- Si usbd muere, el supervisor lo relanza y fsd ve errores de E/S hasta
  entonces; el endpoint de bloques es de init y sobrevive.
