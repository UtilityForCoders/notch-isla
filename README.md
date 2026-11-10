# notch-isla

Isla dinámica (estilo Dynamic Island) para Windows 10/11, escrita en Rust.
Objetivo: controlar la música que suena en el PC (Spotify, YouTube, etc.) desde una píldora en el borde superior de la pantalla, consumiendo lo mínimo posible.

## Estado

| Paso | Qué hace | Estado |
|---|---|---|
| 1 | Píldora arriba al centro que se expande con rebote al hacer clic | Hecho |
| 2 | Leer la canción que suena (título, artista, carátula) vía SMTC | Hecho |
| 3 | Controles: play/pausa, siguiente, anterior | Hecho |
| 4 | Ícono en la bandeja, inicio con Windows, una sola instancia, ocultar en pantalla completa y mantener la barrita sin música | Hecho |

## Requisitos

- Windows 10 (1809+) u 11, 64 bits.
- [Rust](https://rustup.rs) con el toolchain `stable-x86_64-pc-windows-msvc`.
- Visual Studio Build Tools con la carga de trabajo **"Desarrollo para el escritorio con C++"** (aporta el enlazador `link.exe`).

## Ejecutar

```powershell
cargo run            # desarrollo con renderizado optimizado y consola para mensajes
cargo run --release  # versión optimizada, sin consola
```

El ejecutable final queda en `target\release\notch-isla.exe`.

El perfil de desarrollo conserva símbolos y comprobaciones de debug, con optimización de la app y sus dependencias para que el rasterizado no limite las animaciones. Para comparar la fluidez o medir el consumo final, usa `cargo run --release`.

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

- En reposo la isla deja solo una barrita en el borde superior, incluso sin música. Pasar el puntero o hacer clic sobre ella recupera la píldora encogida; otro clic abre los controles. Con **Expandir al pasar el puntero**, los controles se abren automáticamente. Puede ocultarse en pantalla completa si activas esa opción.
- Al sacar el puntero de los controles, la isla se encoge tras 800 ms. La píldora permanece tres segundos y luego vuelve a la barrita superior; volver a pasar el puntero cancela la espera. **Fijar versión extendida** mantiene los controles abiertos mientras se elija esa opción. En la vista extendida, las pestañas **Música** y **Compartir** cambian entre el reproductor y los accesos a archivos, portapapeles, vinculación y equipos guardados. El indicador de recepción de Compartir permite activarla o pausarla; el resultado de las acciones aparece en el propio panel. **Ctrl + Alt + N** también permite ocultarla y volver a mostrarla; el atajo se puede cambiar o desactivar desde la bandeja.
- La píldora encogida muestra la mini carátula y barras de audio que siguen el sonido real de la app que suena. Si hay avisos pendientes, reserva ese espacio para sus iconos. Al dejar de usarla, en reposo queda únicamente la barrita con el avance de la canción.
- Haz clic sobre la píldora para expandirla: en **Música** aparecen la carátula, el título, el artista y los botones anterior / play-pausa / siguiente. Ese primer clic solo abre los controles. Al retirar el puntero se encoge tras una breve espera. Si prefieres la apertura automática, activa **Expandir al pasar el puntero** en la bandeja.
- En la esquina de la carátula, el ícono de la app que suena (Spotify, Chrome...). Si hay varias apps con música, unos puntos bajo la carátula lo indican y un clic en ella pasa a la siguiente.
- Debajo, una barra de progreso con el color de la carátula; haz clic en ella para saltar a ese punto de la canción (si la app informa el tiempo).
- Gira la rueda del mouse sobre la isla para subir o bajar el volumen de la app que suena (como en el Mezclador de volumen de Windows); con **Ctrl** apretado cambia el volumen general. La barra muestra el nivel un momento. Si no se encuentra el sonido de la app, la rueda cambia el volumen general. En los navegadores el volumen es el de todo el navegador, no el de una pestaña.
- **Clic derecho sobre la isla**: elige por qué salida suena la app (por ejemplo, Spotify en los audífonos y el resto en los parlantes), o devuélvela a la predeterminada del sistema. Windows lo recuerda para esa app. Usa una función interna de Windows (la misma del Mezclador de volumen) sin documentación oficial: si una actualización de Windows la cambia, el menú lo indica y el resto de la isla sigue funcionando igual. Algunas apps tardan unos segundos en pasarse a la nueva salida o necesitan pausar y reanudar.
- **Compartir cerca**: vincula tus equipos una sola vez en la misma red local. Abre **Compartir cerca… → Vincular equipo…** en ambos PCs; en uno, selecciona el otro equipo (o pulsa **Buscar de nuevo**) y confirma que el código coincida en ambas pantallas. La vinculación para equipos nuevos se cierra tras dos minutos o al terminar; los equipos guardados pueden seguir enviando mientras Notch Isla esté abierto y **Recibir de equipos vinculados** esté activado. Esta preferencia se conserva al reiniciar. Para enviar, arrastra fotos o archivos sobre la isla, usa **Elegir fotos o archivos…** o **Enviar texto o imagen copiada** y elige uno de **Tus dispositivos**, con estado **Disponible** o **Sin conexión**. El texto y las imágenes individuales de hasta 32 MB muestran una vista previa con **Copiar**; los demás archivos requieren **Guardar en Descargas** o **Rechazar**. **Tus dispositivos → [equipo] → Olvidar equipo** revoca su acceso local; el otro equipo conserva su propia lista hasta que también lo olvides allí. La identidad y la confianza se guardan cifradas con DPAPI en `%LOCALAPPDATA%\NotchIsla\share.dat`, sin cuentas ni servidor. Admite hasta 20 equipos vinculados, 20 archivos y 1 GB por envío, o 256 KB de texto. Actualiza ambos PCs: las versiones anteriores del protocolo de compartir no se admiten para evitar recibir sin vinculación.
- **Letra sincronizada** (opcional): en **Configuración → Música y letra**, **Mostrar letra de la canción** tiene dos opciones que se activan por separado. **Con la isla expandida** muestra la letra en la pestaña Música, como un carrusel que sube un verso cada vez que empieza el siguiente, solo cuando la canción tiene letra sincronizada disponible; durante la búsqueda o si no hay letra, conserva el reproductor compacto sin una columna vacía (también se activa con el botón de letra de la isla, que solo aparece cuando hay letra sincronizada confirmada). **Con la isla encogida** muestra el verso que se canta en un "proyector" bajo la isla: una píldora negra que se ajusta al largo de cada verso, que aparece desvaneciéndose y que sigue a la isla (también cuando se oculta en el borde). Los clics la atraviesan, así que no tapa nada. La letra se pide a [LRCLIB](https://lrclib.net) una vez por canción (se envían título, artista, álbum y duración; ver [PRIVACY.md](PRIVACY.md)). Funciona con las apps que informan el avance de la canción, como Spotify.
- Si el título o el artista no caben, se desplazan de un lado al otro.
- **Salida de audio**: al cambiar a otra salida (audífonos, parlantes), aparece una tarjeta debajo de la isla con su nombre. Si la isla está oculta, la tarjeta aparece sola cerca del borde superior.
- Los avisos de batería baja y actualización se integran en los extremos de la barrita. Al abrir la isla aparecen como iconos en la cabecera: pulsa la **batería naranja** para ver los dispositivos con carga baja y su nivel; el **icono azul de descarga** permite actualizar.
- **Dispositivos y batería**: en **Configuración → Dispositivos**, muestra los periféricos Bluetooth conectados, los mandos que informan batería a Windows y los receptores USB compatibles. Se consulta en segundo plano cada 5 segundos con los avisos de conexión activados, o cada 30 si se desactivan; también se puede **Actualizar ahora**. **Avisar al conectar un dispositivo** muestra su nombre cuando se confirma un nuevo enlace. Un dongle enchufado sin el periférico respondiendo no dispara el aviso. Con una lectura actual de 20 % o menos, la isla avisa una vez por descarga; los niveles aproximados de un mando se indican como tales. El aviso se puede desactivar desde ese apartado. Si el mouse duerme, el último nivel válido queda marcado como **anterior** y no genera avisos. Compatibilidad y pruebas: [docs/battery.md](docs/battery.md).
- Clic derecho en el ícono de la bandeja: muestra únicamente **Configuración…** y **Salir**. Configuración abre una ventana independiente con las categorías **General**, **Apariencia**, **Música y letra**, **Dispositivos**, **Temporizador**, **Compartir** y **Actualizaciones**. Los cambios se guardan automáticamente. Se puede navegar con Tab, Mayús + Tab, flechas y Enter; las flechas izquierda/derecha ajustan el deslizador seleccionado. La rueda permite recorrer las listas largas; Esc o el botón de cierre cierran la ventana. No tiene un temporizador de refresco continuo.
  - **Pantalla**: en qué monitor va la isla (si hay más de uno).
  - **Temporizador**: de 5 a 60 minutos; al terminar suena y la isla avisa.
  - **Desenfoque del fondo**: una barra permite ajustar la intensidad de 0 a 100 % en vivo, para la isla y la letra flotante. Arrástrala o usa las flechas izquierda y derecha con la barra seleccionada. La intensidad se guarda al cerrar la configuración. Se puede desactivar para usar negro opaco. Windows compone el fondo en vivo; si el efecto no está disponible, se usa el fondo opaco.
  - **Expandir al pasar el puntero**: permite volver al modo de apertura sin clic; desactivado por defecto para evitar expansiones accidentales.
  - **Avisar al cambiar la salida de audio**, **Ocultar en pantalla completa**, **Iniciar con Windows** y **Salir**.
  - Los ajustes se guardan en `HKEY_CURRENT_USER\Software\NotchIsla`.
- Al minuto de abrirse y luego cada 6 horas, consulta en GitHub si hay una versión nueva (o al momento, con **Buscar actualizaciones** en la bandeja). Si la app está instalada, descarga el instalador en segundo plano, lo verifica (tamaño y SHA-256 publicados por GitHub) y avisa: un clic en el aviso, o en **Instalar la versión X** del menú de la bandeja, actualiza sin ventanas y la isla vuelve a abrirse sola. La isla avisa "Instalando la versión X" antes de cerrarse y, al volver, "Notch Isla se actualizó" o "No se pudo instalar la versión X". Si la instalación falla, un ayudante (una copia temporal de la app) vuelve a abrir la isla igual, así nunca queda cerrada sin aviso. La versión portable solo avisa y abre la página de descarga. Ver [PRIVACY.md](PRIVACY.md).

Al pulsar **Instalar**, la isla vuelve a consultar el último release y prepara su instalador en segundo plano. Si se publicó otra versión después del aviso, instala esa versión más reciente. Si la consulta falla, no ejecuta el instalador anterior; si la descarga falla, abre la página del último release.

## Cómo está hecha

- **Ventana layered** (`UpdateLayeredWindow`) con transparencia por píxel: los píxeles transparentes dejan pasar los clics a las ventanas de abajo.
- Dibujo con [tiny-skia](https://github.com/linebender/tiny-skia) (anti-aliasing, curvas) sobre un mapa de bits en memoria.
- Animación con un **resorte amortiguado** (`src/spring.rs`). El temporizador de ~60 fps solo existe mientras la isla se mueve: **en reposo, 0 % de CPU**.
- Consciente del DPI por monitor (nítida con escalado 125 %, 150 %…).
- **Música por SMTC** (`Windows.Media.Control`) con eventos, sin sondeo. Los eventos llegan en otros hilos y se pasan a la ventana con `PostMessageW`.
- **Carátula** decodificada con WIC (`Windows.Graphics.Imaging`), recortada al centro.
- **Texto** con DirectWrite, dibujado por Direct2D en modo software sobre el mismo mapa de bits.
- **Menús propios** dibujados igual que la isla (ventana layered, tiny-skia y DirectWrite), en lugar del menú clásico de Windows. Los grupos abren submenús laterales sin agrandar el principal. Se abren con el puntero, un clic o la flecha derecha; izquierda o Esc vuelve al nivel anterior. Se puede seguir usando el menú principal sin cerrarlo.
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

## Medir rendimiento

Con la isla abierta, ejecuta `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/medir-rendimiento.ps1 -Segundos 20 -IncluirGpu`. Muestra CPU, memoria y muestras de uso GPU de la isla y DWM. Agrega `-IdProceso <PID>` para elegir una copia concreta, por ejemplo la compilada localmente.

Compara el mismo estado con desenfoque activado y desactivado. El uso de DWM incluye todo el escritorio; la suma de motores GPU puede superar el 100 %. Estas muestras no miden FPS ni tiempos de presentación. Sin contadores GPU disponibles, el script sigue midiendo CPU y memoria.

Para medir la cadencia del renderizado real, la siguiente prueba muestra durante cinco segundos una isla de prueba que se expande y contrae con letra y desenfoque. Usa contenido local y publica el intervalo entre cuadros y el tiempo de dibujo; no mide la presentación final en GPU:

```powershell
$env:NOTCH_ANIMATION_BENCH = '1'
cargo test --release stability_tests::isla_con_desenfoque_sigue_ocultandose_y_recibiendo_clics -- --ignored --exact --nocapture --test-threads=1
Remove-Item Env:\NOTCH_ANIMATION_BENCH
```

## Compilación en GitHub Actions

`.github/workflows/build.yml` compila el `.exe` y el instalador en cada cambio de `main` y deja los archivos como artefactos de la ejecución. Cuando el repo tenga configurada la firma con SignPath, el mismo flujo firma primero el `.exe`, arma el instalador con él y firma el instalador.

## Licencia

[MIT](LICENSE).
