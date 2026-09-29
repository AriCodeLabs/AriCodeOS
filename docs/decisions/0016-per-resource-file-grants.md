# ADR 0016: Permisos por recurso: ficheros compartidos de solo lectura

- **Fecha:** 2026-09-28
- **Estado:** Aceptada

## Contexto

El manifiesto (Fase 28) concede capacidades por servicio: `fs` da un
directorio privado entero. No había forma de dar a una aplicación acceso a
un fichero concreto de otro sitio, ni de darlo solo en lectura. El estudio
de 2026-09-28 (etapa 3, apartado C) pide empezar por "un archivo
seleccionado en modo lectura" con la restricción aplicada por el servidor,
no por el manifiesto: escribir el permiso no basta para hacerlo efectivo.

## Decisión

1. **`read_files = a, b` en el manifiesto** (hasta 4 nombres completos de
   AriFS). El broker pide a fsd un endpoint por fichero (`Op::Share`, solo
   por el endpoint principal) y lo entrega tras el directorio privado, en
   el orden del manifiesto. Si el fichero no existe, el lanzamiento se
   deniega antes de crear el proceso.
2. **fsd aplica la restricción.** Un endpoint compartido es un espacio de
   nombres de un solo fichero: `List` muestra solo ese fichero (con su
   nombre sin prefijo), `Open` solo lo abre a él, `Read` y `Close`
   funcionan, y `Create`, `Write`, `Replace`, `Flush`, `Bind` y `Share`
   responden `Denied`. Los descriptores siguen perteneciendo al endpoint
   que los abrió (auditoría del SDK).
3. **La autoridad va con el objeto**, no con la identidad: dos aplicaciones
   con el mismo manifiesto reciben duplicados del mismo endpoint
   compartido; una con directorio privado y otra con fichero compartido
   reciben objetos distintos. Cerrar o perder el endpoint de una no afecta
   al resto.

## Alternativas descartadas

- Concesión por identidad de cliente en fsd: exige que fsd conozca quién
  llama y una política duplicada; el modelo de capabilities ya expresa el
  permiso con el endpoint.
- Un flag de solo lectura en `Bind` (directorio entero en lectura): más
  amplio de lo que pide la etapa y no cubre "un fichero de fuera".

## Consecuencias

- `sandbox.shared_file`: `notes_reader` (manifiesto `read_files =
  hola.txt`) lista y lee el fichero, recibe `Denied` al escribir y no ve
  `README`; `hello_app` en la misma sesión sigue con su directorio privado.
- Escribir en un fichero compartido, compartir directorios completos y un
  selector de ficheros para conceder en tiempo de sesión son etapas
  posteriores; el mecanismo (`Share` con una restricción por endpoint) es
  el mismo.
