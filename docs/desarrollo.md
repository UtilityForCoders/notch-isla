# Desarrollo de Nocho

Todo lo necesario para compilar, probar y publicar Nocho. Para usar la app, mira el [README](../README.md).

Las reglas de ramas, commits y versiones están en [CLAUDE.md](../CLAUDE.md) (Git Flow y Conventional Commits).

## Requisitos

- Windows 10 (1809+) u 11, 64 bits.
- [Rust](https://rustup.rs) con el toolchain `stable-x86_64-pc-windows-msvc`.
- Visual Studio Build Tools con la carga de trabajo **"Desarrollo para el escritorio con C++"** (aporta el enlazador `link.exe` y el `rc.exe` del Windows SDK).

## Ejecutar

```powershell
cargo run            # desarrollo con renderizado optimizado y consola para mensajes
cargo run --release  # versión optimizada, sin consola
```

El ejecutable final queda en `target\release\nocho.exe`. Solo puede haber una isla abierta: cierra la versión instalada antes de abrir la compilada.

El perfil de desarrollo conserva símbolos y comprobaciones de debug, con optimización de la app y sus dependencias para que el rasterizado no limite las animaciones. Para comparar la fluidez o medir el consumo final, usa `cargo run --release`.

## Pruebas

```powershell
cargo fmt --check
cargo clippy --release --all-targets -- -D warnings
cargo test --release
```

Algunas pruebas están ignoradas por defecto porque abren ventanas reales (isla fuera de pantalla, configuración, pantalla completa) o miden tiempos. Se corren con `--ignored` y el nombre de la prueba, por ejemplo:

```powershell
cargo test --release stability_tests::isla_abierta_vuelve_a_la_franja_al_salir_de_pantalla_completa -- --ignored --exact --nocapture
cargo test --release tray::settings_tests::revisar_configuracion -- --ignored --exact
```

## Instalador

`installer\build.ps1` compila la app y genera `target\installer\nocho-setup-<versión>.exe` con [Inno Setup 6](https://jrsoftware.org/isinfo.php). La versión sale de `Cargo.toml`.

```powershell
winget install --id JRSoftware.InnoSetup -e --scope user   # una sola vez
.\installer\build.ps1
```

El instalador no pide permisos de administrador: instala en `%LOCALAPPDATA%\Programs\Nocho`, crea el acceso del menú Inicio (y del escritorio, si se elige), ofrece **Iniciar con Windows** y queda en *Aplicaciones instaladas* con su desinstalador, que cierra la isla y quita el inicio automático.

Conserva el AppId de Notch Isla, así actualiza encima de ella: la cierra si está abierta, borra su carpeta y sus accesos directos y quita su valor de inicio. Los ajustes y datos los migra la app al abrirse (`src/migration.rs`).

## Ícono

El ícono (`assets/nocho.ico`) sale del logo `assets/nocho.png` (fondo transparente) y se incrusta en el `.exe` al compilar (`build.rs` + `app.rc`). El instalador usa el mismo archivo. Para regenerarlo después de cambiar el logo:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\generar-icono.ps1
```

El script centra el logo, lo agranda hasta ocupar casi todo el cuadro y guarda los tamaños de 16 a 256 px que usa Windows.

## Dónde guarda las cosas

- Ajustes: `HKEY_CURRENT_USER\Software\Nocho`. Ahí va también el agua anotada en el día (`WaterDay` y `WaterMl`).
- Iniciar con Windows: el valor `Nocho` en `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`.
- Compartir cerca: identidad y equipos vinculados cifrados con DPAPI en `%LOCALAPPDATA%\Nocho\share.dat`; mensajes recibidos pendientes en `%LOCALAPPDATA%\Nocho\share-inbox.json`.

Hasta la 0.8 la app se llamaba Notch Isla y usaba `Software\NotchIsla`, el valor `NotchIsla` y `%LOCALAPPDATA%\NotchIsla`. Nocho los migra al abrirse.

## Actualizaciones

Al minuto de abrirse y luego cada 6 horas, la app consulta el último release en GitHub. Si está instalada, descarga `nocho-setup-x.y.z.exe` en segundo plano y lo verifica contra el tamaño y el SHA-256 que publica GitHub. Al pulsar **Instalar** vuelve a consultar el último release (por si salió otro), anota la versión que instala y lanza una copia temporal de sí misma como ayudante, que corre el instalador en silencio y vuelve a abrir la isla aunque la instalación falle. Al abrirse, la isla avisa si se actualizó.

Cada release publica además una copia del instalador llamada `notch-isla-setup-x.y.z.exe`: es el nombre que buscan Notch Isla 0.3.1 a 0.8 para actualizarse solas. Las rutas viejas del repositorio (`gportal21/notch-isla` y `UtilityForCoders/notch-isla`) redirigen a `UtilityForCoders/Nocho`, así que no hay que volver a crear un repositorio con esos nombres.

## Cómo está hecha

- **Ventana layered** (`UpdateLayeredWindow`) con transparencia por píxel: los píxeles transparentes dejan pasar los clics a las ventanas de abajo. Solo se envía a Windows la zona que cambió.
- Dibujo con [tiny-skia](https://github.com/linebender/tiny-skia) (anti-aliasing, curvas) sobre un mapa de bits en memoria.
- Animación con un **resorte amortiguado** (`src/spring.rs`). El temporizador solo existe mientras algo se mueve, y al ritmo que eso necesita: **en reposo, 0 % de CPU**. Con pantallas de más de 60 Hz se sincroniza con el refresco del monitor.
- Consciente del DPI por monitor (nítida con escalado 125 %, 150 %…).
- **Música por SMTC** (`Windows.Media.Control`) con eventos, sin sondeo. Los eventos llegan en otros hilos y se pasan a la ventana con `PostMessageW`.
- **Carátula** decodificada con WIC (`Windows.Graphics.Imaging`), recortada al centro.
- **Texto** con DirectWrite, dibujado por Direct2D en modo software sobre el mismo mapa de bits.
- **Barras de audio reales**: captura el audio de la app que suena (WASAPI loopback por proceso) y lo separa en bandas con una FFT. Solo corre mientras se ven las barras.
- **Menús propios** dibujados igual que la isla, en lugar del menú clásico de Windows.
- **Pantalla completa** detectada con el aviso de appbar del Explorador (`ABN_FULLSCREENAPP`) y una comprobación de la ventana en primer plano.

```
src/
├─ main.rs           ventana, mensajes de Windows, bucle de animación
├─ island.rs         geometría y dibujo de la isla, botones e indicadores
├─ workspace.rs      cabecera de pestañas y panel de Compartir
├─ spring.rs         física del resorte
├─ media.rs          música vía SMTC: sesiones, carátula y órdenes de reproducción
├─ appicon.rs        ícono de la app que suena (y el logo propio)
├─ text.rs           textos con DirectWrite, carrusel de la letra
├─ lyrics.rs         letra sincronizada de LRCLIB (formato LRC)
├─ projector.rs      letra flotante bajo la isla (ventana que no recibe clics)
├─ visualizer.rs     barras reales: captura de audio y FFT
├─ tray.rs           ícono en la bandeja y sus categorías de configuración
├─ settings_ui.rs    ventana de Configuración
├─ menu.rs           menús con el estilo de la isla
├─ hotkey.rs         atajo de teclado para mostrar u ocultar la isla
├─ settings.rs       ajustes guardados en el registro del usuario
├─ migration.rs      paso de los datos de Notch Isla a Nocho
├─ monitors.rs       pantallas conectadas, para elegir dónde va la isla
├─ system.rs         aviso de cambio de salida de audio
├─ audio.rs          volumen de la app que suena o general (rueda del mouse)
├─ routing.rs        salida de audio por app
├─ battery.rs        baterías de periféricos Bluetooth, mandos y receptores USB
├─ share*.rs         Compartir cerca: red local, vinculación, envíos y vistas previas
├─ timer.rs          temporizador, cronómetro y Pomodoro (solo las cuentas)
├─ water.rs          meta diaria de agua y conteo del día
├─ autostart.rs      iniciar con Windows
├─ update.rs         actualizaciones desde GitHub (WinHTTP)
└─ fullscreen.rs     ocultar la isla cuando una app está a pantalla completa
```

Compatibilidad y pruebas de baterías: [battery.md](battery.md).

## Medir rendimiento

Con la isla abierta:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/medir-rendimiento.ps1 -Segundos 20 -IncluirGpu
```

Muestra CPU, memoria y muestras de uso GPU de la isla y DWM. Agrega `-IdProceso <PID>` para elegir una copia concreta, por ejemplo la compilada localmente. Compara el mismo estado con desenfoque activado y desactivado. El uso de DWM incluye todo el escritorio y la suma de motores GPU puede superar el 100 %. Sin contadores GPU disponibles, el script sigue midiendo CPU y memoria.

Para medir la cadencia del renderizado real, esta prueba muestra durante cinco segundos una isla de prueba que se expande y contrae con letra y desenfoque, y publica el intervalo entre cuadros y el tiempo de dibujo:

```powershell
$env:NOCHO_ANIMATION_BENCH = '1'
cargo test --release stability_tests::isla_con_desenfoque_sigue_ocultandose_y_recibiendo_clics -- --ignored --exact --nocapture --test-threads=1
Remove-Item Env:\NOCHO_ANIMATION_BENCH
```

## Compilación y publicación (GitHub Actions)

`.github/workflows/build.yml`:

- En cada PR comprueba que la rama vaya al destino correcto y que el título siga Conventional Commits.
- En PRs, `develop` y `main` revisa el formato, pasa clippy, corre las pruebas y compila el `.exe` y el instalador.
- En `main`, si la versión de `Cargo.toml` no tiene etiqueta, crea la etiqueta y el release con el instalador, el `.exe` portable y la copia del instalador con el nombre viejo. Las notas salen de los commits `feat`, `fix` y `perf` (`.github/scripts/notas-release.sh`).

Cuando el repositorio tenga configurada la firma con SignPath, el mismo flujo firma primero el `.exe`, arma el instalador con él y firma el instalador.
