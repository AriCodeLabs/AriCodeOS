# Flujo de trabajo Git (git flow)

`main` nunca recibe commits directos. Todo el trabajo entra por ramas y se
integra en `develop`; `main` solo avanza con versiones etiquetadas.

## Ramas

```text
main        -> siempre arrancable; solo merges desde develop (release) o hotfix/*; cada merge lleva tag vX.Y.Z
develop     -> rama de integración; siempre debe pasar `cargo xtask test`
feature/*   -> nueva funcionalidad, parte de develop, vuelve a develop
fix/*       -> corrección de un defecto en develop, parte de develop, vuelve a develop
hotfix/*    -> corrección urgente sobre una versión publicada, parte de main, vuelve a main Y develop
release/*   -> (opcional) estabilización previa a un tag; parte de develop, vuelve a main y develop
experiment/*-> prototipos que pueden romperse; nunca se mergean tal cual
```

Nombres en minúsculas con guiones y, cuando aplique, la fase del roadmap:

```text
feature/f04-gdt-idt
feature/f05-physical-memory
fix/loader-identity-map-limit
hotfix/serial-init-order
```

## Ciclo de una feature

```bash
git checkout develop && git pull --ff-only
git checkout -b feature/f04-gdt-idt
# ... commits pequeños y con mensaje descriptivo ...
cargo xtask check && cargo xtask test        # obligatorio antes de integrar
git checkout develop
git merge --no-ff feature/f04-gdt-idt        # --no-ff: la rama queda visible en el historial
git branch -d feature/f04-gdt-idt
```

## Publicar una versión

```bash
git checkout develop && cargo xtask check && cargo xtask test
git checkout main
git merge --no-ff develop
git tag -a v0.0.2 -m "AriCodeOS 0.0.2: memoria y logging"
git checkout develop
```

## Niveles de test por rama

Los tests viven en el repositorio (`tests/README.md`) y tienen tres niveles
para que cada fase evalúe lo más rápido posible sin perder cobertura:

```text
en una feature/* o fix/*   cargo xtask test --level unit           (segundos, en cada commit)
                           cargo xtask test --area <áreas de la fase>   (solo sus escenarios QEMU)
antes de mergear a develop cargo xtask check && cargo xtask test   (unit + smoke)
antes de mergear a main    cargo xtask check && cargo xtask test --all   (unit + full)
```

Cada fase del roadmap añade sus tests en la misma rama que su código: nuevos
`#[cfg(test)]` en las crates y nuevos `tests/qemu/*.toml` con su área.

## Reglas

1. Ningún commit directo en `main` ni en `develop`: siempre desde una rama.
2. `--no-ff` en todos los merges de integración.
3. Antes de mergear: `cargo xtask check` y el nivel de test que exige la rama
   destino (tabla anterior) en verde.
4. Un tema por rama. Si aparece un bug ajeno durante una feature, se abre `fix/*`.
5. Cada fase terminada (Definition of Done, sección 98 del documento maestro)
   se integra en `develop` y, si cierra una versión de la sección 64, se
   etiqueta en `main`.
6. Decisiones de arquitectura: ADR en `docs/decisions/` dentro de la misma rama.
7. Código factorizado: lógica reutilizable en `libs/*` (no_std, con tests de
   host); las crates bare-metal solo integran.
8. Sin código muerto ni obsoleto: cuando una corrección sustituye a otra cosa,
   la anterior se borra en el mismo commit. `clippy -D warnings` lo vigila.
9. El comportamiento de test se pide por cmdline (`qemu_exit`, `test=...`),
   nunca con features de compilación (ADR 0007).

## Mensajes de commit

Primera línea en imperativo, corta, con el ámbito cuando ayude:

```text
kernel: instalar GDT y TSS propias
ariboot: limitar identidad a memoria no reservada
docs: ADR 0007 sobre formato de la IDT
```
