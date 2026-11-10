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
| R | Volver al último punto seguro, perdiendo velocidad y turbo |
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
el foco, la partida se pausa automáticamente. Las barreras contienen el kart;
tras un segundo seguido fuera del asfalto vuelve al último punto seguro.
Hay que soltar la dirección mantenida para volver a girar después de la
recuperación. Los desplazamientos por fuera no avanzan los controles de vuelta.
El circuito se dibuja en cualquier dirección, incluso al mirar hacia atrás,
y los eventos pendientes del teclado se procesan en cada imagen.

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
- Carrocerías y cascos curvos con iluminación suave en perspectiva, brillo de
  pintura, reflejos del cielo, neumáticos de 16 lados y llantas metálicas.
- Sombras de contacto mezcladas con el suelo, nubes suaves, montañas nevadas
  con sombreado, vegetación variada, llamas de turbo y chispas de derrape.
- Resplandor y acabado de color antes de dibujar la interfaz.
- Cámara de seguimiento que amplía el campo de visión con la velocidad.

La geometría se rasteriza en la CPU con aritmética entera. Si AriCodeOS dispone
del motor de cálculo Intel Gen7 (Ivy Bridge), Pista ejecuta un programa EU en
la GPU para el resplandor y el acabado de color. Es un renderizador híbrido:
**no es una canalización 3D completa por GPU**, ni añade controladores para
otras familias de tarjetas.

Antes de activarlo, el juego compara una ejecución real de la GPU contra el
resultado de referencia de la CPU, píxel a píxel, incluidos los bordes. Si
no hay un dispositivo compatible, está ocupado, se utiliza `gpu=off`, falla
la comprobación o falla un envío, continúa con el efecto por CPU. Los buffers
de un envío fallido no se reutilizan. La pantalla de pausa indica la ruta
activa y el registro serie informa de la detección y los fallos. QEMU verifica
los casos sin GPU y con GPU desactivada; compilar y pasar las pruebas del host
no demuestra la ejecución sobre una Gen7 física.

La aplicación solicita 16 MiB, incluidos los buffers opcionales de GPU.
El rendimiento en AriCodeOS incluye geometría, efectos, copias y compositor;
las medidas del ejemplo del host excluyen el compositor.

## Compilar y comprobar

Desde la raíz del repositorio:

```sh
cargo test -p aricode-pista --all-features
cargo clippy -p aricode-pista --all-features --all-targets -- -D warnings
cargo xtask app-pack desktop/pista --version 2
cargo xtask test --only gui.pista,gui.pista_controls
```

El paquete se genera en `build/pkgs/pista-2.aripkg`. El escenario `gui.pista`
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
