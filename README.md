# notch-isla

Isla dinámica (estilo Dynamic Island) para Windows 10/11, escrita en Rust.
Objetivo: controlar la música que suena en el PC (Spotify, YouTube, etc.) desde una píldora en el borde superior de la pantalla, consumiendo lo mínimo posible.

## Estado

| Paso | Qué hace | Estado |
|---|---|---|
| 1 | Píldora negra arriba al centro que se expande con rebote al pasar el mouse | Hecho |
| 2 | Leer la canción que suena (título, artista, carátula) vía SMTC | Hecho |
| 3 | Controles: play/pausa, siguiente, anterior | Hecho |
| 4 | Ícono en la bandeja, inicio con Windows, una sola instancia, ocultar en pantalla completa y sin música | Hecho |

## Requisitos

- Windows 10 (1809+) u 11, 64 bits.
- [Rust](https://rustup.rs) con el toolchain `stable-x86_64-pc-windows-msvc`.
- Visual Studio Build Tools con la carga de trabajo **"Desarrollo para el escritorio con C++"** (aporta el enlazador `link.exe`).

## Ejecutar

```powershell
cargo run            # modo debug (abre una consola para ver mensajes)
cargo run --release  # versión optimizada, sin consola
```

El ejecutable final queda en `target\release\notch-isla.exe`.

### Instalador

`installer\build.ps1` compila la app y genera `target\installer\notch-isla-setup-<versión>.exe` con [Inno Setup 6](https://jrsoftware.org/isinfo.php) (la versión sale de `Cargo.toml`):

```powershell
winget install --id JRSoftware.InnoSetup -e --scope user   # una sola vez
.\installer\build.ps1
```

El instalador no pide permisos de administrador: instala en `%LOCALAPPDATA%\Programs\Notch Isla`, crea el acceso del menú Inicio (y del escritorio, si se elige), ofrece **Iniciar con Windows** y queda en *Aplicaciones instaladas* con su desinstalador, que cierra la isla y quita el inicio automático.

### Ícono

El ícono (`assets/icon.ico`) se dibuja por código y se incrusta en el `.exe` al compilar (`build.rs` + `app.rc`, con el `rc.exe` del Windows SDK). Para regenerarlo después de cambiar el diseño en `examples/icon.rs`:

```powershell
cargo run --example icon
```

**Uso:**

- La isla aparece cuando alguna app reproduce música y se oculta si no hay nada o si una app está a pantalla completa.
- Si no la usas, a los 3 s (se cambia en la bandeja) sube y se oculta para no tapar lo que hay debajo (pestañas del navegador, etc.). Queda una línea fina en el borde superior, con el avance de la canción en el color de la carátula; lleva el mouse a esa línea para traer la isla de vuelta. **Ctrl + Alt + N** la oculta o la trae en cualquier momento; el atajo se cambia o se desactiva en la bandeja (**Cambiar atajo...** espera la combinación que presiones: Ctrl, Alt o Win más una tecla).
- Es una píldora flotante arriba al centro. Contraída muestra una mini carátula y barras de audio (con el color de la carátula) que siguen el sonido real de la app que suena: graves, medios y agudos. Se puede volver a la animación simple con **Barras con el sonido real** en la bandeja.
- Pasa el mouse sobre la píldora para expandirla: en una sola fila, carátula, título, artista y los botones anterior / play-pausa / siguiente.
- En la esquina de la carátula, el ícono de la app que suena (Spotify, Chrome...). Si hay varias apps con música, unos puntos bajo la carátula lo indican y un clic en ella pasa a la siguiente.
- Debajo, una barra de progreso con el color de la carátula; haz clic en ella para saltar a ese punto de la canción (si la app informa el tiempo).
- Gira la rueda del mouse sobre la isla para subir o bajar el volumen de la app que suena (como en el Mezclador de volumen de Windows); con **Ctrl** apretado cambia el volumen general. La barra muestra el nivel un momento. Si no se encuentra el sonido de la app, la rueda cambia el volumen general. En los navegadores el volumen es el de todo el navegador, no el de una pestaña.
- **Clic derecho sobre la isla**: elige por qué salida suena la app (por ejemplo, Spotify en los audífonos y el resto en los parlantes), o devuélvela a la predeterminada del sistema. Windows lo recuerda para esa app. Usa una función interna de Windows (la misma del Mezclador de volumen) sin documentación oficial: si una actualización de Windows la cambia, el menú lo indica y el resto de la isla sigue funcionando igual. Algunas apps tardan unos segundos en pasarse a la nueva salida o necesitan pausar y reanudar.
- **Letra sincronizada** (opcional): en la bandeja, **Mostrar letra de la canción** tiene dos opciones que se activan por separado. **Con la isla expandida** muestra la letra dentro de la isla como un carrusel que sube un verso cada vez que empieza el siguiente (también se activa con el botón de letra de la isla). **Con la isla encogida** muestra el verso que se canta en un "proyector" bajo la isla: una píldora negra que se ajusta al largo de cada verso, que aparece desvaneciéndose y que sigue a la isla (también cuando se oculta en el borde). Los clics la atraviesan, así que no tapa nada. La letra se pide a [LRCLIB](https://lrclib.net) una vez por canción (se envían título, artista, álbum y duración; ver [PRIVACY.md](PRIVACY.md)). Funciona con las apps que informan el avance de la canción, como Spotify.
- Si el título o el artista no caben, se desplazan de un lado al otro.
- **Salida de audio**: al cambiar a otra salida (audífonos, parlantes), la isla baja un momento con su nombre, aunque no haya música. Un clic cierra el aviso.
- Clic derecho en el ícono de la bandeja:
  - **Ocultarse sola tras**: 3, 5, 10 o 30 segundos, o nunca.
  - **Pantalla**: en qué monitor va la isla (si hay más de uno).
  - **Temporizador**: de 5 a 60 minutos; al terminar suena y la isla avisa.
  - **Desenfoque del fondo**: suave, medio, intenso o muy intenso, para la isla y la letra flotante. Se puede desactivar para usar negro opaco. Windows compone el fondo en vivo; si el efecto no está disponible, se usa el fondo opaco.
  - **Avisar al cambiar la salida de audio**, **Ocultar en pantalla completa**, **Iniciar con Windows** y **Salir**.
  - Los ajustes se guardan en `HKEY_CURRENT_USER\Software\NotchIsla`.
- Al minuto de abrirse y luego cada 6 horas, consulta en GitHub si hay una versión nueva (o al momento, con **Buscar actualizaciones** en la bandeja). Si la app está instalada, descarga el instalador en segundo plano, lo verifica (tamaño y SHA-256 publicados por GitHub) y avisa: un clic en el aviso, o en **Instalar la versión X** del menú de la bandeja, actualiza sin ventanas y la isla vuelve a abrirse sola. La isla avisa "Instalando la versión X" antes de cerrarse y, al volver, "Notch Isla se actualizó" o "No se pudo instalar la versión X". Si la instalación falla, un ayudante (una copia temporal de la app) vuelve a abrir la isla igual, así nunca queda cerrada sin aviso. La versión portable solo avisa y abre la página de descarga. Ver [PRIVACY.md](PRIVACY.md).

## Cómo está hecha

- **Ventana layered** (`UpdateLayeredWindow`) con transparencia por píxel: los píxeles transparentes dejan pasar los clics a las ventanas de abajo.
- Dibujo con [tiny-skia](https://github.com/linebender/tiny-skia) (anti-aliasing, curvas) sobre un mapa de bits en memoria.
- Animación con un **resorte amortiguado** (`src/spring.rs`). El temporizador de ~60 fps solo existe mientras la isla se mueve: **en reposo, 0 % de CPU**.
- Consciente del DPI por monitor (nítida con escalado 125 %, 150 %…).
- **Música por SMTC** (`Windows.Media.Control`) con eventos, sin sondeo. Los eventos llegan en otros hilos y se pasan a la ventana con `PostMessageW`.
- **Carátula** decodificada con WIC (`Windows.Graphics.Imaging`), recortada al centro.
- **Texto** con DirectWrite, dibujado por Direct2D en modo software sobre el mismo mapa de bits.
- **Menús propios** dibujados igual que la isla (ventana layered, tiny-skia y DirectWrite), en lugar del menú clásico de Windows. Los grupos se despliegan dentro del mismo menú; se usa con el mouse o con las flechas, Enter y Esc.
- **Pantalla completa** detectada registrando la ventana como appbar (`ABN_FULLSCREENAPP`), también sin sondeo.

```
src/
├─ main.rs        ventana, mensajes de Windows, bucle de animación
├─ island.rs      geometría y dibujo de la isla, botones e ícono de la bandeja
├─ spring.rs      física del resorte
├─ media.rs       música vía SMTC: sesiones, carátula y órdenes de reproducción
├─ appicon.rs     ícono de la app que suena (shell de Windows o su .exe)
├─ text.rs        título y artista con DirectWrite, desplazamiento de textos largos
├─ tray.rs        ícono en la bandeja y su menú
├─ menu.rs        menús con el estilo de la isla (bandeja y clic derecho sobre la isla)
├─ hotkey.rs      atajo de teclado para mostrar u ocultar la isla (cambiable o desactivable)
├─ settings.rs    ajustes del menú, guardados en el registro del usuario
├─ monitors.rs    pantallas conectadas, para elegir dónde va la isla
├─ system.rs      aviso de cambio de salida de audio (eventos de Core Audio)
├─ audio.rs       volumen de la app que suena o general, con Core Audio (rueda del mouse)
├─ routing.rs     salida de audio por app (interfaz interna de Windows, con respaldo si falta)
├─ autostart.rs   iniciar con Windows (clave Run del usuario)
├─ visualizer.rs  barras reales: captura del audio de la app (WASAPI loopback) y FFT
├─ lyrics.rs      letra sincronizada de LRCLIB (formato LRC)
├─ projector.rs   proyector de la letra bajo la isla (ventana que no recibe clics)
├─ update.rs      actualizaciones: último release en GitHub, descarga e instalación (WinHTTP)
└─ fullscreen.rs  ocultar la isla cuando una app está a pantalla completa
```

## Compilación en GitHub Actions

`.github/workflows/build.yml` compila el `.exe` y el instalador en cada cambio de `main` y deja los archivos como artefactos de la ejecución. Cuando el repo tenga configurada la firma con SignPath, el mismo flujo firma primero el `.exe`, arma el instalador con él y firma el instalador.

## Licencia

[MIT](LICENSE).
