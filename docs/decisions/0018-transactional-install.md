# ADR 0018: Instalación y actualización transaccional de paquetes

- **Fecha:** 2026-09-29
- **Estado:** Aceptada

## Contexto

Con ADR 0017 un programa instalado es un paquete firmado en
`apps/NOMBRE.aripkg`, pero solo xtask sabía ponerlo ahí. La etapa 4 pide
que una aplicación se actualice desde el propio sistema sin pérdida de
datos ni ampliación silenciosa de permisos, y que un corte durante la
instalación no deje un estado a medias. El `replace` atómico de AriFS (ADR
0014) estaba acotado a una petición IPC (4 KiB), menor que un paquete.

## Decisión

1. **Reemplazo atómico por trozos.** `FileServiceProtocol` gana
   `ReplaceChunk(fd, offset, len)` y `ReplaceCommit(fd, len)`: los trozos
   van al área de datos del journal sin comprometer nada; el compromiso
   calcula el CRC de lo que hay en el journal, escribe la cabecera pendiente
   y aplica. `FsClient::replace` trocea solo cuando hace falta: hasta la
   capacidad del journal (32 KiB) con la misma garantía todo-o-nada. Un
   solo reemplazo puede estar a medias en fsd (`Busy` para los demás); lo
   libera su compromiso, el cierre de su descriptor o, si su dueño
   desaparece, dos segundos sin trozos (auditoría 6ee19fb, hallazgo 3).
   Una transacción empieza en el desplazamiento 0 y sigue sin huecos, y el
   compromiso debe cubrir exactamente lo que ella escribió: la librería lo
   comprueba (`BadTransaction`, `Invalid` por IPC), así que un compromiso
   nunca puede llevarse restos del journal de otra aplicación (auditoría
   6ee19fb, hallazgo 1).
2. **Instalador de confianza** (`apps/installer`, capacidad `install` solo
   para él, como `block` lo es para `updater`): recibe el espacio principal
   de fsd, recorre `pkgs/*.aripkg`, verifica cada paquete con la clave de
   desarrollo, lee la versión instalada en `apps/NOMBRE.aripkg` (0 si no
   hay o no verifica) e instala solo versiones más nuevas con un único
   `replace`. Lo que no verifica o no es más nuevo se ignora y se anuncia.
   No hay downgrade: volver atrás exige un paquete con versión mayor.
3. **El broker no cambia**: sigue verificando el paquete al lanzar, así
   que la autoridad la da la firma, no quién escribió el fichero. Los datos
   de la aplicación (su directorio privado en fsd) no se tocan al
   actualizar.

## Alternativas descartadas

- Escribir el paquete nuevo con `write` y renombrar: AriFS no tiene
  renombrado ni borrado, y dos ficheros distintos duplicarían el espacio.
- Ampliar el mensaje IPC o el buffer del cliente al tamaño del paquete: la
  garantía debe ser del sistema de archivos, no del tamaño de un mensaje.
- Elegir el paquete por argumento numérico del shell: `run` solo pasa un
  número; un directorio de paquetes pendientes es lo que un sistema de
  actualización consume de todas formas.

## Consecuencias

- `apps.update`: v1 instalada, `pkgs/` con v2 y una v3 de firma alterada;
  el instalador instala la 2 e ignora la 3; tras reiniciar sigue la 2 y
  volver a instalar no hace nada.
- `apps.update_cut`: corte al empezar la instalación; al volver el paquete
  instalado (v1 o v2) se lanza y el instalador completa la actualización.
- Un paquete instalado no puede pasar de 32 KiB (el journal); los programas
  del SDK ocupan la mitad. Ampliar el journal o encadenar varios es la
  opción cuando haga falta.
- Un instalador que muera con un reemplazo a medias deja el journal
  ocupado hasta que fsd se reinicie; con un solo instalador de confianza
  es aceptable y queda anotado.
