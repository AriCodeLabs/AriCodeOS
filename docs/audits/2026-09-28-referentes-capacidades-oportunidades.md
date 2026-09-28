# Referentes, capacidades y oportunidades para AriCodeOS

Fecha: 2026-09-28. Base: `4836023e6e4e4afa66125106a18f457107ae70bf`, rama
`fix/audit-4836023`, con modificaciones locales en curso.

Estudio comparativo y propuesta de desarrollo; no es una nueva auditoría de
correcciones ni acredita que se hayan ejecutado pruebas en este turno. Las
implementaciones sugeridas no se han aplicado. Las fuentes externas describen
los referentes; las prioridades, diseños y criterios de aceptación son propuestas
para AriCodeOS, no prestaciones atribuidas a esos proyectos.

La oportunidad principal es convertir la arquitectura actual en un sistema que
se pueda usar, diagnosticar y recuperar de manera predecible. Rust, microkernel,
capabilities y WASM tienen precedentes: la diferenciación deberá demostrarse en
experiencia de uso, recuperación, rendimiento y facilidad para crear aplicaciones.

## 1. Qué podemos aprender de otros proyectos

| Referente | Capacidades documentadas | Aprendizaje aplicable y límites |
| --- | --- | --- |
| Genode Labs: [Genode/Sculpt](https://genode.org/download/sculpt) | Microkernel, capabilities, drivers aislados, máquinas virtuales y una interfaz que muestra componentes y relaciones. | Hacer visibles las conexiones entre servicios y sus permisos. Una vista administrativa podría servir para configurar y diagnosticar AriCodeOS. No implica adoptar sus kernels ni su infraestructura completa. |
| Google: [Fuchsia, capabilities](https://fuchsia.dev/fuchsia-src/concepts/components/v2/capabilities) | Los componentes declaran lo que ofrecen y consumen; el entorno establece rutas explícitas y dependencias. | Evolucionar el manifiesto desde una lista de permisos hacia contratos entre servicios, con dependencias obligatorias y opcionales. |
| Oxide: [Hubris](https://hubris.oxide.computer/) | Rust, tareas aisladas, reinicio de drivers y depuración con Humility, incluida inspección del sistema y volcados. | Diseñar recuperación y diagnóstico junto con cada driver. Su configuración estática para microcontroladores no se traslada directamente a un escritorio dinámico. |
| QNX: [herencia de prioridad en IPC](https://qnx.com/developers/docs/8.0/com.qnx.doc.neutrino.sys_arch/topic/ipc_Priority_inheritance_messages.html) | El trabajo de los servidores recibe prioridades relacionadas con las de los clientes para limitar la inversión de prioridad. | Cuando AriCodeOS incorpore prioridades, considerar también las cadenas de llamadas IPC. No basta con cambiar el scheduler; tampoco demostraría tiempo real estricto por sí solo. |
| Ant Group y colaboradores: [Asterinas/OSTD](https://www.usenix.org/conference/atc25/presentation/peng-yuke) | Framework Rust que separa las operaciones de bajo nivel del código de servicios. | Reforzar APIs seguras para memoria, DMA y MMIO. Su [framekernel](https://asterinas.github.io/book/kernel/the-framekernel-architecture.html) comparte espacio de direcciones: copiar esa arquitectura eliminaría una propiedad importante de nuestro aislamiento actual. |
| Comunidad: [Redox OS](https://github.com/redox-os/book/blob/master/src/introduction.md) | Sistema de propósito general en Rust, microkernel y orientación Unix/POSIX. | Referente para estudiar el esfuerzo de SDK, herramientas y aplicaciones. Compatibilidad selectiva puede facilitar ports sin convertir POSIX en la arquitectura del kernel. |
| Investigación: [LionsOS/UNSW](https://lionsos.org/) | Componentes sobre seL4, separación entre drivers y multiplexación, colas y configuración principalmente estática. | Separar transporte de hardware, política y clientes. LionsOS se declara experimental y no tiene todavía una historia completa de verificación; no debe confundirse con las pruebas del kernel seL4. |

Otros dos referentes transversales: [WASI](https://wasi.dev/) define interfaces
portables y un modelo de autoridad explícita para aplicaciones Wasm;
[TUF](https://theupdateframework.io/docs/overview/) aborda autenticidad y frescura
de actualizaciones, así como el impacto de claves comprometidas. Son útiles para
evitar protocolos privados innecesarios y completar el modelo de actualización.

## 2. Qué tenemos y qué significa realmente

La base local consultada es [arquitectura](../architecture.md),
[documento maestro](../../AriCodeOS.md), [SDK](../sdk.md),
[hardware](../hardware.md), manifiestos y protocolos. «Implementado» aquí
significa presente en el código/documentación, no madurez de producción.

| Área | Base existente | Siguiente escalón |
| --- | --- | --- |
| Kernel | Rust `no_std`, UEFI, x86-64, memoria protegida, procesos, IPC, SMP. | Recurso por proceso, rendimiento bajo carga y validación física. |
| Autoridad | Handles con derechos, revocación, broker, manifiestos y directorio privado de app. | Permisos por recurso/operación, concesiones temporales, política editable y explicación de denegaciones. |
| Servicios y drivers | Drivers en ring 3, supervisión desde init, virtio, xHCI/HID y VT-d. | Recuperación de clientes, cierre de canales, hotplug y política cuando falta aislamiento DMA. |
| Aplicaciones | SDK Rust y runtime propio; intérprete WASM con límites de pasos/profundidad. | Validación previa de módulos, interfaces útiles, compatibilidad y herramientas para terceros. |
| Actualizaciones | Imágenes firmadas, slots A/B, intentos y rollback. | Secuencia completa entre arranques, cortes de energía, salud del sistema y migración de datos. |
| Escritorio | Compositor, superficies, foco y entrada. | Terminal gráfico, texto internacional, portapapeles controlado y uso real con teclado USB. |
| Almacenamiento y red | AriFS inicial, bloques virtio, IPv4/DHCP/TCP/DNS/HTTP. | Persistencia ante fallos, almacenamiento físico, red estable y TLS. |
| Validación | Bibliotecas comprobables en host y escenarios QEMU con inyección de fallos. | Pruebas de duración, hardware, regresiones de rendimiento y fallos entre componentes. |

Hay trabajo local para filtrar xHCI, comprobar que los servicios de entrada
responden antes de publicarlos y limitar las esperas de `InputClient`, además de
correcciones HID. Debe revisarse y probarse antes de dar por cerrada la
[auditoría 4836023](2026-09-28-usb-hid-4836023.md). La propuesta siguiente
generaliza ese trabajo; no lo considera ausente.

La documentación contiene estados antiguos: README anuncia 0.1.0, la cabecera del
documento maestro 0.0.7 y arquitectura 0.5.1; algunos puntos de «Deuda conocida»
contradicen funciones ya descritas. Conviene mantener una tabla única con
`propuesto / prototipo / validado en QEMU / validado en hardware` y enlazar sus
pruebas. Una fase «completada» no significa que sus extensiones también lo estén.

## 3. Mejoras prioritarias y cómo implementarlas

Las prioridades siguientes expresan orden de desarrollo, no severidad de bugs.
El esfuerzo es relativo: pequeño, medio o grande; no es una estimación de fechas.

### A. Contrato común de vida y recuperación de servicios

**Prioridad inmediata; esfuerzo medio/grande.** La supervisión ya existe.
Ampliarla con estados `Starting`, `Ready`, `Degraded`, `Stopping` y `Failed`,
plazos de arranque, dependencias y un máximo de reinicios con espera creciente.
Un servicio opcional ausente debe permitir continuar en modo degradado.

En [init](../../services/init/src/main.rs), generalizar el mecanismo de entrada a
un supervisor reutilizable. Introducir identidad de instancia —una generación
de servicio distinta de la generación de los handles— y reconexión de clientes.
Definir cómo se detiene el dispositivo, se retiran IRQ/DMA y se recuperan recursos
antes de reiniciarlo. Si no puede detenerse DMA con garantías, mantener el
dispositivo aislado y no reutilizar prematuramente sus buffers.

**Aceptación:** provocar 100 reinicios de un driver de prueba mientras otro
servicio sigue funcionando; comparar memoria, handles y mapeos antes/después.
Comprobar tanto recuperación como agotamiento del límite de reinicios.

### B. Semántica IPC de fallos, plazos y reintentos

**Prioridad inmediata; esfuerzo medio/grande.** Ya hay timeouts y protocolos
versionados. Falta convertirlos en un contrato uniforme del SDK.

En [protocolos](../../sdk/proto/src/lib.rs) y clientes, añadir identificador de
petición, generación del servidor y plazo total. Definir cierre del lado servidor,
despertar de clientes y respuesta tardía. La cancelación debe distinguir
«no se ejecutó» de «resultado desconocido»; un timeout no deshace una operación.
Para escrituras, evitar reintentos automáticos sin un mecanismo de deduplicación
o transacción persistente. Para lecturas, documentar cuándo puede reintentarse.

**Aceptación:** retrasar una respuesta hasta después del timeout y emitir otra
petición; nunca confundir ambas. Reiniciar un servidor durante una escritura y
devolver un resultado definido, sin prometer ejecución exactamente una vez.

### C. Manifiestos con recursos concretos y dependencias

**Prioridad siguiente; esfuerzo medio/grande.** Hoy
[Manifest](../../libs/manifest/src/lib.rs) declara `name`, `caps` y `memory`;
la cuota de memoria se refiere a buffers DMA. Extender con versión de esquema,
servicios requeridos/opcionales, derechos y límites independientes.

Empezar con un archivo seleccionado en modo lectura y red por servicio/destino;
el broker entrega un endpoint restringido y el servidor aplica la restricción.
Escribir el permiso en el manifiesto no basta para hacerlo efectivo. Mantener la
autorización ligada a objetos y rutas; una identidad de cliente por sí sola no
otorga autoridad. La concesión efectiva será la intersección de petición, política
y capacidades disponibles. Inspiración: las rutas de Fuchsia citadas arriba.

**Aceptación:** dos apps reciben objetos distintos, una solo lectura; cambiar la
política afecta a nuevas concesiones según contrato, y revocar una concesión no
revoca accidentalmente recursos ajenos. Una dependencia opcional ausente no
impide arrancar; ciclos obligatorios se rechazan antes del lanzamiento.

### D. Cuotas completas y respuesta bajo saturación

**Prioridad siguiente; esfuerzo grande.** CPU y handles ya están pendientes en
el roadmap. Añadir límites de colas IPC, procesos, memoria compartida y recursos
retenidos por servidores en nombre de un cliente. Definir quién paga un buffer
transferido y reservar recursos para diagnóstico y recuperación.

Empezar por contadores y rechazo predecible en asignación; después presupuestos
de CPU. Si se introducen prioridades, estudiar propagación en llamadas IPC y
límites para que una app no se eleve indefinidamente. QNX aporta un precedente,
no una implementación que pueda copiarse sin adaptar su semántica.

**Aceptación:** una app que satura CPU, handles o peticiones recibe límites
observables y otra sigue progresando dentro del objetivo medido de latencia.

### E. Diagnóstico consultable, no solo mensajes de serie

**Prioridad inmediata en versión mínima; esfuerzo medio.** La observabilidad ya
figura en el roadmap. Implementar un esquema acotado de estado por servicio:
versión, generación, salud, último error, reinicios, colas, memoria y tiempos.
Empezar con `services --detail`, `caps` y exportación de un informe desde el shell;
la interfaz gráfica puede venir después.

Fuchsia ofrece [Inspect](https://fuchsia.dev/fuchsia-src/development/diagnostics/inspect)
para publicar estado estructurado; Hubris aporta el ejemplo de inspección y
volcados. Para AriCodeOS proponemos un buffer circular con identificadores de
petición y build, sin contenido privado de archivos ni mensajes por defecto.
El acceso al estado de otros procesos requerirá una capability de diagnóstico.

**Aceptación:** distinguir «espera hardware», «espera servicio» y «agotó cuota»
desde un informe; desactivar el lector no bloquea al productor ni hace crecer
memoria sin límite. Conservar símbolos del build permite interpretar el volcado.

### F. Actualización completa y datos recuperables

**Prioridad inmediata para las pruebas; implementación posterior por etapas;
esfuerzo grande.** A/B y firmas ya existen. Añadir un escenario que conserve
el mismo disco, instale B, reinicie, confirme B o vuelva a A. Inyectar interrupciones
en cada fase de escritura y verificar datos, no solo mensajes de arranque.

La confirmación debe depender de servicios esenciales y una operación de lectura/
escritura, no únicamente de que el kernel esté vivo. Versionar también los datos
de apps: un rollback del sistema no revierte automáticamente una migración.
Antes de distribuir por red, diseñar rotación de claves y metadatos de versiones
con TUF como referente. Separar recuperación autorizada de rechazo de versiones
obsoletas; cualquier política de caducidad necesita una fuente de tiempo y una
regla explícita para equipos sin reloj fiable.

**Aceptación:** cada corte lleva a una imagen válida o a recuperación explícita,
sin confirmar una instalación incompleta ni destruir los datos anteriores.
La firma de imagen no equivale a una cadena UEFI Secure Boot: la guía actual
requiere desactivarlo. El cargador y la ruta de recuperación también forman parte
de la futura cadena de confianza.

### G. Drivers y almacenamiento para una máquina concreta

**Prioridad inmediata de planificación; esfuerzo grande.** Elegir el portátil
de referencia y registrar PCI IDs, firmware, almacenamiento, USB e IOMMU.
Implementar el controlador de almacenamiento que necesite esa máquina; AHCI,
NVMe y USB mass storage son proyectos separados.

El firmware puede leer el pendrive para arrancar, pero eso no da al kernel acceso
posterior al pendrive ni al disco. USB HID tampoco proporciona almacenamiento USB.
Para entrada, añadir desconexión/reconexión, identificación individual, limpieza
de teclas retenidas y después hubs y descriptores HID más generales.

**Aceptación:** completar un arranque y sesión de entrada en el equipo elegido;
probar persistencia sobre un soporte de pruebas cuando exista su driver. Publicar
qué funciona en QEMU, qué funciona físicamente y qué modo carece de IOMMU.

### H. WASM útil para aplicaciones de terceros

**Prioridad siguiente; esfuerzo grande.** El intérprete ya limita pasos,
profundidad y memoria, pero no valida completamente el módulo antes de ejecutarlo
y su API importada es mínima. Priorizar validador, manifiesto por módulo y recursos
concedidos al módulo, separados de los privilegios del runner.

Implementar primero archivos, reloj y una interfaz de aplicación pequeña;
seleccionar un perfil WASI explícito y comprobarlo con herramientas externas.
La documentación consultada ya describe WASI 0.3 y Component Model: no asumir que
un módulo MVP actual lo soporta por añadir unas funciones. Comparar extender el
intérprete con integrar un runtime compatible tras estudiar `no_std`, memoria,
portabilidad, licencia y mantenimiento. JIT y SIMD pueden esperar a las métricas.

**Aceptación:** ejecutar la misma app de prueba en AriCodeOS y un runtime de
referencia usando exactamente el perfil declarado; rechazar módulos inválidos y
accesos no concedidos. Una llamada importada bloqueante también necesita plazo:
el contador de instrucciones no limita por sí solo ese tiempo.

## 4. Áreas que conviene añadir o concretar en el backlog

No se afirma que todas estén ausentes del proyecto. La columna «situación»
separa previsiones existentes de detalles que este estudio propone desarrollar.

| Área / situación | Implementación útil | Cómo comprobar el resultado |
| --- | --- | --- |
| Persistencia: AriFS inicial | Contrato de `flush`, escrituras parciales, reemplazo atómico y recuperación; elegir journal o copy-on-write mediante ADR. | Cortes durante datos y metadatos, disco lleno y corrupción detectada. A/B no sustituye estas garantías. |
| Red: deuda ya documentada | Renovación DHCP, cambios de enlace, DNS con plazos, buffers limitados y mejora gradual de TCP. TLS como biblioteca/servicio evaluado. | Pérdida/reordenación, cambio de dirección y reconexión. HTTPS exige confianza de certificados, tiempo y entropía. |
| Energía: ampliar roadmap | Reloj monotónico, batería, temperatura, estados de reposo y suspensión/reanudación con contratos por driver. | Medir consumo y reanudar conservando temporizadores, DMA y estado de entrada coherentes. |
| Escritorio: GUI inicial | Terminal gráfico conectado a USB, edición de línea, fuente Unicode, distribución española, foco y redimensionado. | Escribir, editar y guardar un archivo sin consola serie; probar ñ, tildes y pérdida de foco. |
| Accesibilidad: concretar diseño | Escalado, alto contraste, navegación por teclado y árbol semántico para futuras ayudas. | Completar operaciones principales sin ratón; evitar que toda la UI sea solo píxeles sin semántica. |
| Permisos de escritorio: ampliar sandbox | Selector de archivos que entregue acceso al archivo elegido; portapapeles, captura de pantalla y entrada global mediados. | Una app sin concesión no observa otras ventanas; retirar acceso tiene efecto definido. |
| ABI y SDK: ya previstos | Esquema de protocolos, generación de codecs y matriz cliente/servidor por versión. | Compatibilidad N/N−1 cuando se prometa; versiones desconocidas fallan claramente. [FIDL](https://fuchsia.dev/fuchsia-src/reference/fidl/language/versioning) es un referente, no una garantía automática de ABI. |
| Paquetes: ya previstos | Identidad por editor y contenido, manifiesto firmado, instalación transaccional y separación entre datos y binarios. | Instalar, interrumpir, actualizar y desinstalar sin pérdida accidental de datos ni ampliación silenciosa de permisos. [Fuchsia](https://fuchsia.dev/fuchsia-src/concepts/packages/package) ilustra almacenamiento por contenido. |
| Memoria/CPU: ampliar medidas | Latencia IPC p50/p95/p99, colas, cambios de contexto y tiempo en locks; optimizar después con lotes y colas por CPU. | Comparación con igual build, carga, CPU y modo QEMU/KVM; no extrapolar TCG a hardware. |
| Compartición de memoria: generalizar base existente | Separar buffers CPU compartidos de DMA; objetos tipados con dueño, permisos y duración de mapeos. | Revocación, cierre, transferencia y limpieza tras muerte sin uso de memoria ya liberada. |
| Garantías: `unsafe` ya tiene política | Inventario de operaciones inseguras y contratos para MMIO/DMA; empezar modelos de estados por handles, IPC y actualización. | Pruebas de propiedades e intercalados, revisión de APIs seguras y correspondencia con el código. Las [pruebas de seL4 tienen supuestos explícitos](https://sel4.systems/Verification/assumptions.html); pruebas unitarias no equivalen a verificación formal. |
| Entropía y secretos: base inicial | Fuentes de entropía adicionales, estado «no inicializado», API que indique indisponibilidad y almacenamiento de claves separado por app. | Comportamiento sin RDRAND y sin fuente fiable; no presentar TSC como garantía criptográfica. |
| Cadena de construcción: concretar operación | Builds reproducibles, dependencias fijadas, procedencia de artefactos, inventario de licencias y símbolos por versión. | Dos builds controlados producen el mismo artefacto sin firma; explicar diferencias de herramientas y metadatos. |
| CI y pruebas: roadmap existente | Ejecutor unit/QEMU con tiempo y espacio acotados, retención de evidencias y reproducción por commit. | Un fallo conserva logs y comando; no agota disco. No se ha encontrado `.github/`, pero eso no descarta CI externa. |
| Compatibilidad: ya prevista | Portar unas aplicaciones mediante SDK/WASM; valorar libc parcial o VM para necesidades concretas. | Matriz de programas que realmente funcionan y coste de mantenimiento; posponer POSIX completo y Windows. |
| Audio, Wi-Fi y GPU: ya contemplados | Elegir hardware y casos de uso; audio necesita plazos y permisos, Wi-Fi necesita reconexión/credenciales, GPU gestión de buffers. | Una app y un dispositivo de referencia por subsistema antes de ampliar soporte. |

## 5. Ideas para diferenciar AriCodeOS

Son propuestas de producto, no afirmaciones de novedad mundial.

1. **Un sistema que explica un bloqueo.** Un comando o panel muestra la cadena
   `app → servicio → driver`, el recurso esperado y el plazo. Requiere contratos
   IPC y diagnóstico; empezaría como herramienta de texto, con acceso restringido.
2. **Permisos para una tarea concreta.** El usuario selecciona un documento y
   concede lectura a una app durante esa sesión. Se apoya en un selector de
   confianza y endpoints restringidos; no requiere conceder todo el directorio.
3. **Recuperación visible y comprobable.** Al reiniciar un driver, el sistema
   informa qué funciones se recuperaron y cuáles perdieron estado. Una demo
   reproducible puede demostrar continuidad de otras aplicaciones sin ocultar
   interrupciones ni prometer recuperar una escritura desconocida.
4. **Modo de diagnóstico desde USB.** Un entorno mínimo inventaría hardware,
   explica qué drivers faltan y exporta un informe local. Empezar por pantalla y
   serie; exportar a un archivo requerirá un driver de almacenamiento operativo.
5. **SDK con ejecución en el host.** Implementaciones simuladas de servicios
   permiten desarrollar y probar lógica de apps en Linux; QEMU comprueba ABI,
   aislamiento y comportamiento real. Reduce el coste de participar en el proyecto.
6. **Perfil de sistema reproducible.** Describir servicios, versiones, rutas y
   límites en una configuración validada al construir. Usar el mismo perfil para
   la imagen y las pruebas; más adelante visualizarlo como hace Sculpt.
7. **Informe de salud por versión.** Publicar tiempos, consumo, recuperación,
   dispositivos probados y límites conocidos junto a cada imagen. Separar lo
   demostrado de lo previsto haría las comparaciones mucho más creíbles.

## 6. Secuencia recomendada

| Etapa | Resultado entregable | Dependencias / puerta de salida |
| --- | --- | --- |
| 0. Consolidar | Revisar los cambios locales de USB, ejecutar regresiones y actualizar el estado de capacidades. | Auditoría resuelta con evidencia; no deducirlo de que exista un parche. |
| 1. Recuperación observable | Contrato común de servicios, peticiones correlacionadas, diagnóstico mínimo y pruebas de muerte/reinicio. | Sin esperas huérfanas; recursos regresan al nivel esperado; estado degradado explícito. |
| 2. Una sesión útil | Terminal con USB, equipo de referencia y ruta de persistencia seleccionada y probada. | Escribir y recuperar un archivo, apagar/reiniciar y reproducir la sesión en el equipo soportado. |
| 3. Límites y actualización | Cuotas adicionales, permisos por recurso, cortes de escritura y actualización entre arranques. | Otra app sigue funcionando bajo carga; rollback conserva datos compatibles. |
| 4. Aplicaciones externas | SDK reproducible, perfil WASM/WASI documentado, paquetes y compatibilidad controlada. | Una app desarrollada fuera del árbol se construye, instala, ejecuta y actualiza. |
| 5. Optimización y ampliación | Rendimiento, energía, más dispositivos, audio y compatibilidad adicional. | Cada ampliación responde a una carga o necesidad concreta y tiene hardware/pruebas de referencia. |

Las investigaciones pueden solaparse; no conviene abrir a la vez nuevos frentes
de GPU, Wi-Fi, navegador, JIT, POSIX completo y múltiples arquitecturas. El primer
objetivo demostrable propuesto es una sesión local útil que sobreviva al fallo
de un componente y pueda actualizarse conservando sus datos.

Cada implementación debe entrar por su propia rama y ADR cuando cambie un
contrato, siguiendo [WORKFLOW](../WORKFLOW.md). Este estudio no cambia el roadmap
oficial ni da por implementada ninguna propuesta.
