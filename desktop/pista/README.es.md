# Pista — Alpine Grand Prix

Carrera arcade de tres vueltas contra tres rivales, integrada en AriCodeOS.
El circuito alpino incluye desniveles, curvas enlazadas y tres zonas de turbo.

## Controles

| Tecla | Acción |
| --- | --- |
| Flechas / WASD | Acelerar, frenar y girar |
| Espacio + giro | Derrapar; soltar Espacio activa el turbo cargado |
| P | Pausar / continuar |
| Enter | Continuar una pausa o empezar otra carrera tras terminar |
| R | Volver al tramo actual, perdiendo velocidad y turbo |
| Q | Alternar calidad alta y modo rápido |
| Esc | Salir |

La carga de derrape se vuelve azul a los 40 pasos de simulación y dorada a los
90. Soltar Espacio concede respectivamente 45 o 90 pasos de turbo. Hay 60 pasos
por segundo. Salir al césped cancela la carga. Empezar a acelerar entre 8 y 30
pasos antes de la salida concede un impulso inicial; mantener el acelerador
durante toda la cuenta atrás no lo concede.

Para completar una vuelta hay que cruzar tres controles en orden y después la
meta en el sentido de carrera. La posición distingue la aproximación inicial
a la salida del final de una vuelta. Al terminar se congelan el tiempo y el
resultado. Los tiempos de vuelta se mantienen durante esa carrera. Al perder
el foco, la partida se pausa automáticamente.

## Gráficos

- Alta: renderizado nativo a 960×540. Rápido: 480×270 ampliado al doble.
- Interfaz a 960×540 en ambos modos: posición, velocidad, mapa, cronómetro,
  mejor vuelta, carga de derrape y resultado.
- Profundidad recíproca por píxel y recorte del plano cercano; los objetos y
  los desniveles se ocultan correctamente entre sí.
- Texturas procedurales de asfalto y césped con coordenadas de perspectiva y
  desvanecimiento de detalle en la distancia.
- Cielo cálido, halo solar, nubes y tres capas de montañas; árboles, rocas,
  gradas, chalés, banderines animados, señales y barreras.
- Karts con neumáticos octogonales, dirección en las ruedas delanteras,
  casco facetado, alerón, llamas de turbo y chispas de derrape.
- Cámara de seguimiento que amplía el campo de visión con la velocidad.

El renderizado utiliza la CPU y aritmética entera, sin dependencias gráficas
externas. La aplicación solicita 16 MiB para la ventana, los buffers de color
y profundidad y el runtime. El rendimiento dentro de AriCodeOS incluye además
la copia del framebuffer y el compositor; las medidas del ejemplo del host
no representan los FPS completos del sistema.

## Compilar y comprobar

Desde la raíz del repositorio:

```sh
cargo test -p aricode-pista
cargo clippy -p aricode-pista --all-targets -- -D warnings
cargo xtask app-pack desktop/pista
cargo xtask test --only gui.pista,gui.pista_controls
```

El paquete se genera en `build/pkgs/pista-1.aripkg`. El escenario `gui.pista`
instala la aplicación y presenta 60 imágenes a través del compositor.
`gui.pista_controls` prueba el recorrido interactivo con cambios de calidad,
pausa, recuperación, conducción y salida mediante el teclado del compositor.
En una instalación que tenga el paquete, se abre con `run pista`.

Para generar capturas reales y medir el renderizador en el host:

```sh
cargo run -p aricode-pista --release --example preview -- build/pista-preview
```

Escribe imágenes PPM de la parrilla y de dos momentos de carrera. No necesita
AriCodeOS arrancado ni librerías de ventana. El benchmark dentro de AriCodeOS
sigue disponible con `run pista 60`.
