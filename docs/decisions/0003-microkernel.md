# ADR 0003: Microkernel pragmático

- **Fecha:** 2026-09-26
- **Estado:** Aceptada

## Decisión

El kernel contiene solo scheduler, memoria virtual, IPC, interrupciones,
capabilities y SMP. Almacenamiento, red, USB, audio, filesystem, compositor y
device manager viven en espacio de usuario.

## Consecuencias

- `fallo de driver != fallo del kernel`; los drivers se pueden reiniciar.
- El IPC es crítico y se mide desde las primeras fases.
- Coste: más cambios de contexto que un monolítico; se optimizará con métricas.
