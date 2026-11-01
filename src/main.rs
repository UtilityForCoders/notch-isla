// En release no se abre consola; en debug sí, para ver mensajes.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appicon;
mod audio;
mod autostart;
mod backdrop;
mod frame_wait;
mod fullscreen;
mod hotkey;
mod island;
mod lyrics;
mod media;
mod menu;
mod monitors;
mod projector;
mod routing;
mod settings;
mod spring;
#[cfg(test)]
mod stability_tests;
mod system;
mod text;
mod tray;
mod ui;
mod update;
mod visualizer;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use audio::Volume;
use island::{Button, Controls, Hit, Notice, NoticeIcon};
use media::NowPlaying;
use projector::Projector;
use settings::Settings;
use spring::Spring;
use system::AudioWatch;
use text::TextRenderer;
use tiny_skia::Pixmap;
use tray::Tray;
use visualizer::Visualizer;
use windows::core::{w, Result};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetMonitorInfoW, SelectObject,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
    HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFO,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::NIN_BALLOONUSERCLICK;
use windows::Win32::UI::WindowsAndMessaging::{
    BeginDeferWindowPos, CreateWindowExW, DefWindowProcW, DeferWindowPos, DestroyWindow,
    DispatchMessageW, EndDeferWindowPos, GetCursorPos, GetMessageW, KillTimer, LoadCursorW,
    PostMessageW, PostQuitMessage, RegisterClassExW, SetTimer, SetWindowPos, ShowWindow,
    TranslateMessage, UpdateLayeredWindow, UpdateLayeredWindowIndirect, HWND_TOPMOST, IDC_ARROW,
    MA_NOACTIVATE, MB_ICONASTERISK, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, UPDATELAYEREDWINDOWINFO, WM_APP,
    WM_CAPTURECHANGED, WM_CONTEXTMENU, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_HOTKEY,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONUP,
    WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

/// Aviso de "el mouse salió de la ventana" (definido aquí para no cargar todo el módulo Win32_UI_Controls).
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Mensaje que envía el hilo de sincronización VSync (DWM) cuando las animaciones de alta tasa de refresco están activas.
const WM_ANIM_TICK: u32 = WM_APP + 9;

const ANIM_TIMER: usize = 1;
/// Temporizador de un solo disparo: oculta la isla tras un rato sin usarla (lo que digan los
/// ajustes; con "Nunca" no se usa).
const IDLE_TIMER: usize = 2;
/// Quita el aviso del sistema que se está mostrando.
const NOTICE_TIMER: usize = 3;
/// El temporizador que el usuario puso desde la bandeja.
const COUNTDOWN_TIMER: usize = 4;
/// Espera entre el aviso "Instalando" y el cierre de la isla para instalar.
const INSTALL_TIMER: usize = 5;
const INSTALL_DELAY_MS: u32 = 1500;
/// Fin de la espera del atajo nuevo (si no se presionó nada).
const CAPTURE_TIMER: usize = 6;
const CAPTURE_MS: u32 = 10_000;
/// Espera antes de encogerse cuando el mouse sale de la isla: si sales sin querer o vuelves
/// enseguida, no se cierra.
const COLLAPSE_TIMER: usize = 7;
const COLLAPSE_DELAY_MS: u32 = 800;
/// Comprobación periódica de pantalla completa (juegos y apps que no avisan por Explorer).
const FULLSCREEN_TIMER: usize = 8;
const FULLSCREEN_CHECK_MS: u32 = 500;

/// Resorte de la expansión (rigidez, amortiguación): rápido, con un pequeño rebote.
const EXPAND_SPRING: (f32, f32) = (260.0, 22.0);
/// Resorte al encogerse: más lento y con la amortiguación justa para no rebotar
/// (2 * raíz de 110 = 21), así la isla se cierra suave y se asienta sin sacudirse.
const COLLAPSE_SPRING: (f32, f32) = (110.0, 21.0);

/// Cuánto dura cada aviso.
const NOTICE_MS: u32 = 2500;
const NOTICE_TIMER_DONE_MS: u32 = 8000;
const NOTICE_UPDATE_MS: u32 = 6000;

/// Colores de los avisos: salida de audio e instalación, temporizador, actualización lista y
/// fallida.
const BLUE: [u8; 3] = [10, 132, 255];
const ORANGE: [u8; 3] = [255, 159, 10];
const GREEN: [u8; 3] = [48, 209, 88];
const RED: [u8; 3] = [255, 69, 58];

/// ~60 fps. El temporizador solo existe mientras hay animación: en reposo la CPU queda en 0%.
const FRAME_MS: u32 = 15;
/// ~30 fps para las barras de audio de la isla contraída: se ven fluidas y cuestan la mitad.
const BARS_MS: u32 = 33;
/// 20 fps para pintar el verso que se canta: el borde avanza unas pocas letras por segundo,
/// así se ve continuo, y cada cuadro con letra es el más caro (hay que dibujar los textos).
const PAINT_MS: u32 = 50;
/// 4 fps para la barra de progreso: avanza pocos píxeles por segundo, no hace falta más.
const PROGRESS_MS: u32 = 250;

/// Cuánto cambia el volumen por cada "clic" de la rueda (como Windows: 2 %).
const VOLUME_STEP: f32 = 0.02;
/// Cuánto tiempo se muestra el nivel de volumen después del último giro de la rueda.
const VOLUME_SHOW: Duration = Duration::from_millis(1200);

/// Qué sigue moviéndose después de dibujar un cuadro.
#[derive(Default)]
struct Motion {
    /// Un texto largo se desplaza (isla expandida): en cuántos ms dibujar el cuadro siguiente.
    scroll_ms: Option<u32>,
    /// Las barras de audio bailan (isla contraída y música sonando).
    bars: bool,
    /// La barra de progreso avanza (isla expandida y música sonando).
    progress: bool,
    /// La línea de tiempo del borde avanza (isla oculta): cada cuántos ms crece un píxel.
    edge_ms: Option<u32>,
    /// Un botón está en su animación de pulsado.
    pressing: bool,
    /// El proyector de la letra se está moviendo (aparece, cambia de tamaño o de verso).
    projecting: bool,
    /// Con letra, cada cuántos ms empieza el verso siguiente (para mostrarlo a tiempo).
    lyric_ms: Option<u32>,
    /// La letra de la isla expandida está subiendo al verso siguiente.
    lyric_sliding: bool,
    /// El verso que se canta se está pintando (isla expandida con letra y música sonando).
    lyric_painting: bool,
    /// La isla cambia entre la forma de aviso y la de la música.
    reshaping: bool,
}

/// Tiempo de la canción como minutos y segundos ("03:07").
fn clock(secs: f32) -> String {
    let secs = secs.max(0.0).round() as u32;
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// Cuánto tarda el carrusel de la letra en subir al verso siguiente.
const LYRIC_SLIDE_SECS: f32 = 0.5;

/// Último verso mostrado en la isla y la animación con la que llegó.
struct LyricSlide {
    /// Canción (`lyrics::key`) y verso que se canta (-1 = antes del primero).
    key: String,
    index: isize,
    /// Versos que faltaban por subir al empezar, y cuándo empezó.
    from: f32,
    at: Instant,
}

impl LyricSlide {
    /// Versos que aún faltan por subir: frena al llegar (ease-out cúbico).
    fn shift(&self, now: Instant) -> f32 {
        let t = now.duration_since(self.at).as_secs_f32() / LYRIC_SLIDE_SECS;
        if t >= 1.0 {
            0.0
        } else {
            self.from * (1.0 - t).powi(3)
        }
    }
}

/// Cuánto le falta al carrusel para llegar a su sitio, en versos. Al pasar al verso siguiente
/// todo arranca un verso más abajo y sube; al volver al anterior (arrastrando la barra hacia
/// atrás), arranca uno más arriba y baja. Si ya venía moviéndose, sigue desde donde estaba.
/// Un salto de varios versos u otra canción se muestra directo, sin animación.
fn slide_shift(slide: &mut Option<LyricSlide>, key: &str, index: isize) -> f32 {
    let now = Instant::now();
    match slide {
        Some(s) if s.key == key && s.index == index => {}
        Some(s) if s.key == key && (s.index - index).abs() == 1 => {
            let step = (index - s.index) as f32;
            let from = (s.shift(now) + step).clamp(-2.0, 2.0);
            s.index = index;
            s.from = from;
            s.at = now;
        }
        _ => {
            *slide = Some(LyricSlide {
                key: key.to_string(),
                index,
                from: 0.0,
                at: now,
            })
        }
    }
    slide.as_ref().map_or(0.0, |s| s.shift(now))
}

fn main() -> Result<()> {
    unsafe {
        // Copia temporal lanzada para actualizar: espera al instalador y vuelve a abrir la isla.
        let args: Vec<String> = std::env::args().collect();
        if args.get(1).map(String::as_str) == Some(update::UPDATER_FLAG) {
            update::run_updater(&args[2..]);
            return Ok(());
        }

        // Una sola isla: si ya hay otra abierta, esta se cierra sin hacer nada.
        // El mutex vive mientras `_instance` exista, es decir, hasta que termine `main`.
        let _instance = CreateMutexW(None, false, w!("Local\\NotchIsla.Instancia"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(());
        }

        // COM en el hilo de la ventana: lo necesita Core Audio (volumen con la rueda).
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        // Nítida en pantallas con escalado (125 %, 150 %...).
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let instance = GetModuleHandleW(None)?;
        let class_name = w!("NotchIslaWindow");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(windows::core::Error::from_thread());
        }

        // Ventana sin bordes, transparente por píxel, siempre encima, sin botón en la barra de tareas
        // y que nunca roba el foco a la app que estás usando.
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_name,
            w!("Notch Isla"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        let settings = Settings::load();
        let monitor = monitors::find(&settings.monitor);
        let mut app = App::new(hwnd, monitor, settings)?;
        app.render();
        APP.with(|cell| *cell.borrow_mut() = Some(app));

        // La ventana empieza oculta: aparece cuando SMTC reporte música (ver `update_visibility`).
        media::start(hwnd);
        fullscreen::register(hwnd);
        let _ = SetTimer(Some(hwnd), FULLSCREEN_TIMER, FULLSCREEN_CHECK_MS, None);
        update::start(hwnd);
        // Se crean con la app ya guardada: Windows avisa enseguida los valores actuales.
        with_app(|app| {
            app.audio = AudioWatch::new(hwnd).ok();
            app.audio_device = app.audio.as_ref().and_then(AudioWatch::device_name);
        });
        // Si se acaba de instalar una actualización desde la isla, se avisa cómo salió.
        if let Some((ok, version)) = update::take_install_result() {
            with_app(|app| app.install_result(ok, &version));
        }
        // Si otra app ya usa el atajo, simplemente no hay atajo.
        with_app(|app| {
            hotkey::register(hwnd, app.settings.hotkey);
        });

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            ui::promote_timers();
        }

        APP.with(|cell| cell.borrow_mut().take());
    }
    Ok(())
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Ejecuta `f` con el estado de la app si está disponible (evita pánicos por reentrada).
fn with_app(f: impl FnOnce(&mut App)) -> bool {
    let ran = APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => match guard.as_mut() {
            Some(app) => {
                f(app);
                true
            }
            None => false,
        },
        Err(_) => false,
    });
    if ran {
        ui::flush();
    }
    ran
}

/// Estado del arrastre horizontal de la isla por la parte superior.
struct DragIsland {
    start_cursor_x: i32,
    start_pos_x: i32,
    has_moved: bool,
    pending_hit: Option<Hit>,
    last_drag: Instant,
}

/// Conductor de alta tasa de refresco (120 Hz, 144 Hz, 240 Hz): sincroniza las animaciones
/// directamente con el refresco de hardware del monitor mediante DwmFlush() sin la limitación
/// de 10 ms de SetTimer. Duerme completamente (0 % CPU) cuando no hay animaciones activas.
struct HighRefreshDriver {
    active: Arc<(Mutex<bool>, Condvar)>,
    pending: Arc<AtomicBool>,
    interval_ns: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
}

impl HighRefreshDriver {
    fn new(hwnd: HWND) -> Self {
        Self::with_sync(hwnd, || unsafe { monitors::DwmFlush().ok() })
    }

    fn with_sync(hwnd: HWND, sync: fn() -> Result<()>) -> Self {
        let active = Arc::new((Mutex::new(false), Condvar::new()));
        let pending = Arc::new(AtomicBool::new(false));
        let thread_active = Arc::clone(&active);
        let thread_pending = Arc::clone(&pending);
        let interval_ns = Arc::new(AtomicU64::new(FRAME_MS as u64 * 1_000_000));
        let thread_interval = Arc::clone(&interval_ns);
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);
        let hwnd_raw = hwnd.0 as isize;

        std::thread::Builder::new()
            .name("notch-vsync".into())
            .spawn(move || {
                let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
                let waiter = frame_wait::FrameWait::new();
                loop {
                    // Duerme hasta que haya una animación que requiera alta tasa de refresco:
                    {
                        let (lock, cvar) = &*thread_active;
                        let mut is_active = lock.lock().unwrap();
                        while !*is_active && thread_running.load(Ordering::Acquire) {
                            is_active = cvar.wait(is_active).unwrap();
                        }
                        if !thread_running.load(Ordering::Acquire) {
                            return;
                        }
                    }

                    // Sincroniza con el refresco de pantalla del DWM (hardware VSync):
                    let started = Instant::now();
                    let synced = sync().is_ok();
                    // DwmFlush puede volver inmediatamente (composición, sesión remota,
                    // cambios de pantalla). Un éxito no garantiza que haya esperado un VSync.
                    let interval =
                        Duration::from_nanos(thread_interval.load(Ordering::Relaxed).max(1));
                    let elapsed = started.elapsed();
                    // Si DWM ya esperó el refresco, no añadir otra espera: incluso una
                    // fracción de ms puede hacernos perder el siguiente VSync a 240 Hz.
                    if !synced || elapsed < interval / 2 {
                        if let Some(remaining) = interval.checked_sub(elapsed) {
                            waiter.wait(remaining);
                        }
                    }
                    if !thread_running.load(Ordering::Acquire) {
                        return;
                    }

                    let is_active = *thread_active.0.lock().unwrap();
                    if is_active && !thread_pending.swap(true, Ordering::SeqCst) {
                        unsafe {
                            if PostMessageW(
                                Some(hwnd),
                                WM_ANIM_TICK,
                                windows::Win32::Foundation::WPARAM(0),
                                windows::Win32::Foundation::LPARAM(0),
                            )
                            .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
            })
            .expect("iniciar hilo vsync");

        Self {
            active,
            pending,
            interval_ns,
            running,
        }
    }

    fn set_active(&self, active: bool) {
        let (lock, cvar) = &*self.active;
        let mut is_active = lock.lock().unwrap();
        if *is_active != active {
            *is_active = active;
            if active {
                cvar.notify_one();
            }
        }
    }

    fn frame_processed(&self) {
        self.pending.store(false, Ordering::SeqCst);
    }
}

impl Drop for HighRefreshDriver {
    fn drop(&mut self) {
        let mut active = self.active.0.lock().unwrap();
        self.running.store(false, Ordering::Release);
        *active = false;
        self.active.1.notify_one();
    }
}

struct App {
    hwnd: HWND,
    surface: Surface,
    /// Posición de la ventana en pantalla (píxeles físicos).
    pos: POINT,
    scale: f32,
    spring: Spring,
    hovered: bool,
    expanded_by_click: bool,
    /// Intervalo del temporizador de animación, o `None` si está apagado (reposo).
    timer_ms: Option<u32>,
    last_tick: Instant,
    last_vsync: Option<Instant>,
    /// Reloj de las barras de audio.
    started: Instant,
    /// Si DirectWrite fallara, la isla sigue funcionando sin texto.
    text: Option<TextRenderer>,
    media: Option<NowPlaying>,
    /// Botón bajo el mouse, para resaltarlo.
    hovered_button: Option<Button>,
    /// El mouse está sobre la barra de progreso: en qué fracción de la canción.
    seek_hover: Option<f32>,
    /// Se está arrastrando la barra: fracción bajo el mouse. Mientras dura, la barra, los
    /// tiempos y la letra muestran ese momento; el salto se pide al soltar.
    dragging: Option<f32>,
    /// Arrastre horizontal de la isla para colocarla en la parte superior.
    drag_island: Option<DragIsland>,
    /// El mouse está sobre la carátula y se puede cambiar de app.
    cover_hovered: bool,
    /// El mouse está sobre el punto de actualización.
    update_hovered: bool,
    tray: Tray,
    /// Inicio del desplazamiento de textos largos; se reinicia al expandir o cambiar de canción.
    scroll_start: Instant,
    /// Hay una app a pantalla completa en el monitor de la isla.
    fullscreen: bool,
    /// Si la isla debe verse (hay música y no hay pantalla completa).
    visible: bool,
    /// Si la ventana está mostrada de verdad; al ocultarse, sigue visible hasta que termine de salir.
    shown: bool,
    /// Entrada y salida de la pantalla: 0 = escondida arriba, 1 = en su sitio.
    presence: Spring,
    /// Resorte para deslizar suavemente la isla de vuelta al centro al pulsar "Centrar en la pantalla".
    x_spring: Spring,
    /// Transición suave de anchura: 0 = reproductor compacto, 1 = tarjeta amplia con letra.
    lyrics_spring: Spring,
    /// Cambio de forma con los avisos: 0 = tarjeta de la música, 1 = fila del aviso.
    notice_spring: Spring,
    /// Última canción, para dibujarla mientras la isla sale si la música terminó.
    last_media: Option<NowPlaying>,
    /// Control del volumen; se crea la primera vez que se gira la rueda.
    volume: Option<Volume>,
    volume_level: f32,
    /// El nivel mostrado es el de la app que suena (no el general).
    volume_app: bool,
    /// Hasta cuándo se muestra el nivel de volumen en la isla.
    volume_until: Option<Instant>,
    /// Versión nueva publicada en GitHub, si la hay (se ofrece en el menú de la bandeja).
    update: Option<update::Update>,
    /// Se ocultó sola por no usarse; vuelve al tocar el borde superior con el mouse.
    idle_hidden: bool,
    /// Botón recién pulsado y cuándo, para su animación.
    pressed: Option<(Button, Instant)>,
    settings: Settings,
    /// Aviso del sistema en pantalla: ocupa la isla expandida hasta que vence `NOTICE_TIMER`.
    notice: Option<ActiveNotice>,
    /// Último aviso, que se sigue dibujando mientras la isla se contrae o sale.
    last_notice: Option<ActiveNotice>,
    /// Escucha la salida de audio; `None` si Core Audio no está disponible.
    audio: Option<AudioWatch>,
    /// Nombre de la salida de audio actual, para avisar solo cuando de verdad cambia.
    audio_device: Option<String>,
    /// Temporizador en marcha: cuándo termina y de cuántos minutos es.
    countdown: Option<(Instant, u32)>,
    /// Actualización a instalar en cuanto termine el aviso "Instalando".
    installing: Option<update::Update>,
    /// El mouse acaba de salir: sigue expandida hasta que vence `COLLAPSE_TIMER`.
    lingering: bool,
    /// Letras ya consultadas, por canción (`lyrics::key`); vacía = esa canción no tiene.
    lyrics: HashMap<String, Vec<lyrics::Verse>>,
    /// Canciones cuya letra se está pidiendo ahora.
    lyrics_pending: HashSet<String>,
    /// Carrusel de la letra en la isla expandida.
    lyric_slide: Option<LyricSlide>,
    /// Proyector de la letra bajo la isla. Se crea la primera vez que hay un verso que mostrar
    /// (con la letra apagada no ocupa memoria); si no se pudiera crear, la isla sigue igual.
    projector: Option<Projector>,
    backdrop: Option<backdrop::Backdrop>,
    /// Escucha el audio de la app que suena para las barras reales.
    visualizer: Visualizer,
    /// Frecuencia de refresco de la pantalla actual en Hz (60, 120, 240, etc.).
    hz: u32,
    high_refresh_driver: HighRefreshDriver,
}

/// Aviso del sistema con sus textos.
struct ActiveNotice {
    view: Notice,
    title: String,
    subtitle: String,
}

impl App {
    fn new(hwnd: HWND, monitor: HMONITOR, settings: Settings) -> Result<Self> {
        let (surface, pos, scale) = layout_for(monitor, settings.offset_x)?;
        Ok(Self {
            hwnd,
            surface,
            pos,
            scale,
            spring: Spring::new(0.0, EXPAND_SPRING.0, EXPAND_SPRING.1),
            hovered: false,
            expanded_by_click: false,
            timer_ms: None,
            last_tick: Instant::now(),
            last_vsync: None,
            started: Instant::now(),
            text: TextRenderer::new(scale).ok(),
            media: None,
            hovered_button: None,
            seek_hover: None,
            dragging: None,
            drag_island: None,
            cover_hovered: false,
            update_hovered: false,
            tray: Tray::new(hwnd),
            scroll_start: Instant::now(),
            fullscreen: false,
            visible: false,
            shown: false,
            presence: Spring::new(0.0, 260.0, 22.0),
            x_spring: Spring::new(pos.x as f32, 220.0, 23.0),
            lyrics_spring: Spring::new(if settings.lyrics_island { 1.0 } else { 0.0 }, 260.0, 22.0),
            notice_spring: Spring::new(0.0, 260.0, 22.0),
            last_media: None,
            volume: None,
            volume_level: 0.0,
            volume_app: false,
            volume_until: None,
            update: None,
            idle_hidden: false,
            pressed: None,
            settings,
            notice: None,
            last_notice: None,
            audio: None,
            audio_device: None,
            countdown: None,
            installing: None,
            lingering: false,
            lyrics: HashMap::new(),
            lyrics_pending: HashSet::new(),
            lyric_slide: None,
            projector: None,
            backdrop: backdrop::Backdrop::new().ok(),
            visualizer: Visualizer::default(),
            hz: monitors::refresh_rate(monitor),
            high_refresh_driver: HighRefreshDriver::new(hwnd),
        })
    }

    /// Pide la letra de la canción actual si alguna de las dos está activada y aún no se pidió.
    fn request_lyrics(&mut self) {
        if !self.settings.lyrics() {
            return;
        }
        let Some(m) = &self.media else { return };
        // Sin título o sin artista (videos del navegador, por ejemplo) no hay qué buscar.
        if m.title.is_empty() || m.artist.is_empty() {
            return;
        }
        let key = lyrics::key(&m.title, &m.artist);
        if self.lyrics.contains_key(&key) || self.lyrics_pending.contains(&key) {
            return;
        }
        self.lyrics_pending.insert(key);
        lyrics::fetch(
            self.hwnd,
            m.title.clone(),
            m.artist.clone(),
            m.album.clone(),
            m.timeline.map(|t| t.duration),
        );
    }

    /// Llegó la letra de una canción (o la respuesta de que no hay).
    fn set_lyrics(&mut self, found: lyrics::Lyrics) {
        self.lyrics_pending.remove(&found.key);
        self.lyrics.insert(found.key, found.lines);
        self.redraw();
    }

    /// Muestra un aviso del sistema durante `ms`: la isla baja (aunque no haya música o se
    /// haya ocultado sola) y se expande con el aviso. Un aviso nuevo reemplaza al anterior.
    fn show_notice(&mut self, view: Notice, title: &str, subtitle: &str, ms: u32) {
        self.notice = Some(ActiveNotice {
            view,
            title: title.to_string(),
            subtitle: subtitle.to_string(),
        });
        unsafe {
            SetTimer(Some(self.hwnd), NOTICE_TIMER, ms, None);
        }
        self.stop_idle_countdown();
        self.update_visibility();
        self.update_expansion();
        self.redraw();
    }

    fn end_notice(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), NOTICE_TIMER);
        }
        let Some(notice) = self.notice.take() else {
            return;
        };
        self.last_notice = Some(notice);
        self.update_visibility();
        self.update_expansion();
        if self.visible && !self.hovered {
            self.start_idle_countdown();
        }
        self.redraw();
    }

    /// Cambió la salida de audio predeterminada: se avisa su nombre.
    fn on_audio_device(&mut self) {
        let Some(audio) = self.audio.as_ref() else {
            return;
        };
        let name = audio.device_name();
        if name.is_none() || name == self.audio_device {
            return;
        }
        self.audio_device = name.clone();
        if self.settings.notices {
            let view = Notice {
                icon: NoticeIcon::Audio,
                color: BLUE,
            };
            let name = name.unwrap_or_default();
            self.show_notice(view, "Salida de audio", &name, NOTICE_MS);
        }
    }

    /// Pone en marcha el temporizador de la bandeja (reemplaza al anterior).
    fn start_countdown(&mut self, minutes: u32) {
        self.countdown = Some((
            Instant::now() + Duration::from_secs(minutes as u64 * 60),
            minutes,
        ));
        unsafe {
            SetTimer(Some(self.hwnd), COUNTDOWN_TIMER, minutes * 60_000, None);
        }
        let view = Notice {
            icon: NoticeIcon::Timer,
            color: ORANGE,
        };
        self.show_notice(
            view,
            "Temporizador",
            &format!("{minutes} minutos"),
            NOTICE_MS,
        );
    }

    fn cancel_countdown(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), COUNTDOWN_TIMER);
        }
        self.countdown = None;
    }

    /// Clic en "Instalar": avisa que empieza y, tras un momento para leerlo, se cierra para
    /// instalar (ver `INSTALL_TIMER`).
    fn begin_install(&mut self, update: update::Update) {
        if self.installing.is_some() {
            return;
        }
        let title = format!("Instalando la versión {}", update.version);
        self.announce(
            Notice {
                icon: NoticeIcon::Installing,
                color: BLUE,
            },
            &title,
            "La isla se cierra y vuelve en unos segundos",
            INSTALL_DELAY_MS + 2000,
        );
        self.installing = Some(update);
        unsafe {
            SetTimer(Some(self.hwnd), INSTALL_TIMER, INSTALL_DELAY_MS, None);
        }
    }

    /// Al abrirse tras una actualización: avisa si quedó instalada la versión nueva o no.
    fn install_result(&mut self, ok: bool, version: &str) {
        if ok {
            self.announce(
                Notice {
                    icon: NoticeIcon::Done,
                    color: GREEN,
                },
                "Notch Isla se actualizó",
                &format!("Ahora tienes la versión {version}"),
                NOTICE_UPDATE_MS,
            );
        } else {
            let current = env!("CARGO_PKG_VERSION");
            self.announce(
                Notice {
                    icon: NoticeIcon::Failed,
                    color: RED,
                },
                &format!("No se pudo instalar la versión {version}"),
                &format!("Sigues con la {current}; se volverá a ofrecer"),
                NOTICE_UPDATE_MS,
            );
        }
    }

    /// "Buscar actualizaciones": avisa que está buscando y consulta GitHub en otro hilo (el
    /// resultado llega con `WM_UPDATE` o `WM_UPDATE_CHECKED`).
    fn check_updates_now(&mut self) {
        self.announce(
            Notice {
                icon: NoticeIcon::Installing,
                color: BLUE,
            },
            "Buscando actualizaciones",
            "Un momento...",
            NOTICE_UPDATE_MS,
        );
        update::check_now(self.hwnd);
    }

    /// Resultado de "Buscar actualizaciones" cuando no hay versión nueva.
    fn update_checked(&mut self, failed: bool) {
        if failed {
            self.announce(
                Notice {
                    icon: NoticeIcon::Failed,
                    color: RED,
                },
                "No se pudo buscar actualizaciones",
                "Revisa tu conexión a internet",
                NOTICE_UPDATE_MS,
            );
        } else {
            self.announce(
                Notice {
                    icon: NoticeIcon::Done,
                    color: GREEN,
                },
                "Notch Isla está al día",
                &format!("Tienes la última versión ({})", env!("CARGO_PKG_VERSION")),
                NOTICE_MS + 1000,
            );
        }
    }

    /// Aviso en la isla o, si no puede mostrarse (pantalla completa), notificación de Windows.
    fn announce(&mut self, view: Notice, title: &str, subtitle: &str, ms: u32) {
        if self.hidden_by_fullscreen() {
            self.tray.notify(title, subtitle);
        } else {
            self.show_notice(view, title, subtitle, ms);
        }
    }

    /// Se acabó el tiempo: suena y avisa. Si la isla no puede mostrarse (pantalla completa),
    /// el aviso va como notificación de Windows.
    fn countdown_done(&mut self) {
        let minutes = self.countdown.map_or(0, |(_, m)| m);
        self.cancel_countdown();
        unsafe {
            let _ = MessageBeep(MB_ICONASTERISK);
        }
        let subtitle = format!("Temporizador de {minutes} minutos");
        if self.hidden_by_fullscreen() {
            self.tray.notify("Se acabó el tiempo", &subtitle);
            return;
        }
        let view = Notice {
            icon: NoticeIcon::Timer,
            color: ORANGE,
        };
        self.show_notice(view, "Se acabó el tiempo", &subtitle, NOTICE_TIMER_DONE_MS);
    }

    /// Minutos que le quedan al temporizador (redondeando hacia arriba), si hay uno.
    fn countdown_left(&self) -> Option<u32> {
        self.countdown.map(|(end, _)| {
            let secs = end.saturating_duration_since(Instant::now()).as_secs();
            secs.div_ceil(60) as u32
        })
    }

    /// Ctrl + Alt + N: si la isla está, se oculta en el borde; si estaba oculta, vuelve.
    fn toggle_hidden(&mut self) {
        if self.media.is_none() {
            return;
        }
        if self.idle_hidden {
            self.reveal();
        } else {
            self.stop_idle_countdown();
            self.idle_hidden = true;
            self.update_visibility();
        }
    }

    /// El puntero solo expande si se eligió ese modo; un clic abre los controles.
    fn update_expansion(&mut self) {
        let requested = self.expanded_by_click || self.settings.expand_on_hover;
        let expand = self.visible
            && (self.notice.is_some() || (requested && (self.hovered || self.lingering)));
        self.aim_expansion(expand);
    }

    /// Lleva la isla a expandida o contraída, cada una con su movimiento: al expandirse, rápido
    /// y con un pequeño rebote; al encogerse, más lento y sin rebote, frenando suave al final.
    fn aim_expansion(&mut self, expand: bool) {
        let (stiffness, damping) = if expand {
            EXPAND_SPRING
        } else {
            COLLAPSE_SPRING
        };
        self.spring.retune(stiffness, damping);
        self.spring.set_target(if expand { 1.0 } else { 0.0 });
        self.start_animation();
    }

    /// Usa `hotkey` como atajo (o lo desactiva con `None`), lo guarda y avisa. Si otra app ya
    /// usa esa combinación, vuelve `previous`.
    fn set_hotkey(&mut self, hotkey: Option<hotkey::Hotkey>, previous: Option<hotkey::Hotkey>) {
        let keyboard = Notice {
            icon: NoticeIcon::Keyboard,
            color: BLUE,
        };
        if hotkey::register(self.hwnd, hotkey) {
            self.settings.hotkey = hotkey;
            self.settings.save();
            let subtitle = hotkey.map_or("Desactivado".to_string(), |h| h.name());
            self.show_notice(
                keyboard,
                "Atajo para mostrar u ocultar",
                &subtitle,
                NOTICE_MS,
            );
        } else {
            hotkey::register(self.hwnd, previous);
            self.settings.hotkey = previous;
            self.settings.save();
            let name = hotkey.map(|h| h.name()).unwrap_or_default();
            self.show_notice(
                Notice {
                    icon: NoticeIcon::Failed,
                    color: RED,
                },
                &format!("{name} ya lo usa otra app"),
                "El atajo no cambió",
                NOTICE_UPDATE_MS,
            );
        }
    }

    /// "Cambiar atajo...": la isla espera la próxima combinación de teclas.
    fn begin_hotkey_capture(&mut self) {
        // Sin el atajo registrado, también se puede volver a elegir el mismo.
        hotkey::register(self.hwnd, None);
        if !hotkey::start_capture(self.hwnd) {
            hotkey::register(self.hwnd, self.settings.hotkey);
            return;
        }
        unsafe {
            SetTimer(Some(self.hwnd), CAPTURE_TIMER, CAPTURE_MS, None);
        }
        self.show_notice(
            Notice {
                icon: NoticeIcon::Keyboard,
                color: BLUE,
            },
            "Presiona el atajo nuevo",
            "Ctrl, Alt o Win más una tecla; Esc cancela",
            CAPTURE_MS,
        );
    }

    /// Terminó la espera del atajo: `captured` = la combinación, o `None` si se canceló.
    fn end_hotkey_capture(&mut self, captured: Option<hotkey::Hotkey>) {
        hotkey::stop_capture();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), CAPTURE_TIMER);
        }
        match captured {
            Some(h) => self.set_hotkey(Some(h), self.settings.hotkey),
            None => {
                hotkey::register(self.hwnd, self.settings.hotkey);
                let subtitle = self
                    .settings
                    .hotkey
                    .map_or("Desactivado".to_string(), |h| h.name());
                self.show_notice(
                    Notice {
                        icon: NoticeIcon::Keyboard,
                        color: BLUE,
                    },
                    "El atajo no cambió",
                    &subtitle,
                    NOTICE_MS,
                );
            }
        }
    }

    /// Ajustes cambiados desde la bandeja (ya guardados).
    fn apply_settings(&mut self, settings: Settings) {
        let moved = settings.monitor != self.settings.monitor;
        let delay_changed = settings.hide_delay != self.settings.hide_delay;
        let high_refresh_changed = settings.high_refresh != self.settings.high_refresh;
        let previous_hotkey = self.settings.hotkey;
        self.settings = settings;
        if moved {
            self.relayout();
        }
        if high_refresh_changed {
            if !self.settings.high_refresh {
                self.high_refresh_driver.set_active(false);
            }
            if self.timer_ms.is_some() {
                self.set_timer(Some(self.frame_ms()));
            }
        }
        if self.settings.hotkey != previous_hotkey {
            self.set_hotkey(self.settings.hotkey, previous_hotkey);
        }
        // Recién activada: se pide la letra de lo que suena ahora.
        self.request_lyrics();
        self.lyrics_spring
            .set_target(if self.settings.lyrics_island {
                1.0
            } else {
                0.0
            });
        self.start_animation();
        if delay_changed {
            if self.idle_hidden && self.settings.hide_delay == 0 {
                // Con "Nunca", la que estaba oculta vuelve.
                self.reveal();
            } else if self.visible && !self.hovered && self.notice.is_none() {
                self.start_idle_countdown();
            }
        }
        self.update_visibility();
        self.redraw();
    }

    fn hidden_by_fullscreen(&self) -> bool {
        self.fullscreen && self.settings.hide_fullscreen
    }

    /// Cambió la pantalla elegida, la resolución o el DPI: se recoloca en la pantalla de los
    /// ajustes (o en la principal si esa ya no está).
    fn relayout(&mut self) {
        let monitor = monitors::find(&self.settings.monitor);
        self.hz = monitors::refresh_rate(monitor);
        if let Ok((surface, pos, scale)) = layout_for(monitor, self.settings.offset_x) {
            self.surface = surface;
            self.pos = pos;
            self.scale = scale;
            self.x_spring.reset(pos.x as f32);
            if let Some(text) = self.text.as_mut() {
                text.set_scale(scale);
            }
            if let Some(projector) = self.projector.as_mut() {
                projector.set_scale(scale);
            }
            self.render();
        }
    }

    /// Clic en un botón: manda la orden a la app y anima el botón.
    fn press(&mut self, button: Button) {
        if button == Button::Lyrics {
            // El botón está en la isla expandida: cambia la letra de la isla, no la del proyector.
            self.settings.lyrics_island = !self.settings.lyrics_island;
            self.settings.save();
            self.lyrics_spring
                .set_target(if self.settings.lyrics_island {
                    1.0
                } else {
                    0.0
                });
            self.request_lyrics();
        } else {
            media::press(button);
        }
        self.pressed = Some((button, Instant::now()));
        self.start_animation();
    }

    /// Oculta, queda la franja del borde superior (hay música, pero la isla se ocultó sola).
    fn edge_strip(&self) -> bool {
        // Mientras un aviso ocupa la isla, la línea no se dibuja encima.
        self.idle_hidden
            && self.media.is_some()
            && self.notice.is_none()
            && !self.hidden_by_fullscreen()
    }

    /// (Re)inicia la cuenta para ocultarse; se cancela mientras el mouse esté encima.
    fn start_idle_countdown(&mut self) {
        let secs = self.settings.hide_delay;
        if secs == 0 {
            self.stop_idle_countdown();
            return;
        }
        unsafe {
            SetTimer(Some(self.hwnd), IDLE_TIMER, secs * 1000, None);
        }
    }

    fn stop_idle_countdown(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), IDLE_TIMER);
        }
    }

    /// Se acabó la espera sin usarla: sube y deja solo la franja del borde.
    fn idle_hide(&mut self) {
        self.stop_idle_countdown();
        // Con un aviso en pantalla espera: la cuenta vuelve a empezar cuando el aviso termina.
        if self.hovered || !self.visible || self.notice.is_some() {
            return;
        }
        self.idle_hidden = true;
        self.update_visibility();
    }

    /// El mouse tocó la franja del borde: la isla baja (contraída) y vuelve a contar.
    fn reveal(&mut self) {
        if !self.idle_hidden {
            return;
        }
        self.idle_hidden = false;
        self.update_visibility();
        self.start_idle_countdown();
    }

    /// Rueda del mouse sobre la isla: sube o baja el volumen de la app que suena (o el general,
    /// con Ctrl o si no se encuentra el sonido de la app) y lo muestra un momento.
    fn wheel(&mut self, delta: i16, general: bool) {
        if !self.visible {
            return;
        }
        if self.volume.is_none() {
            self.volume = Volume::new().ok();
        }
        let Some(volume) = &self.volume else { return };
        // `delta` llega en múltiplos de 120 por clic (las ruedas finas mandan fracciones).
        let delta = delta as f32 / 120.0 * VOLUME_STEP;
        let app = match (&self.media, general) {
            (Some(m), false) => volume.change_app(&m.app_id, delta),
            _ => None,
        };
        let level = match app {
            Some(level) => Some(level),
            None => volume.change(delta).ok(),
        };
        if let Some(level) = level {
            self.volume_level = level;
            self.volume_app = app.is_some();
            self.volume_until = Some(Instant::now() + VOLUME_SHOW);
            self.redraw();
        }
    }

    /// Intervalo en milisegundos para cuadros de movimiento continuo.
    /// Si la opción está activada y el monitor supera los 60 Hz (120 Hz, 144 Hz, 240 Hz, etc.),
    /// se adapta a la frecuencia del monitor; si no, mantiene los 15 ms estándar (~60 Hz).
    fn frame_ms(&self) -> u32 {
        if self.settings.high_refresh && self.hz > 60 {
            ((1000.0 / self.hz as f32).round() as u32).clamp(1, FRAME_MS)
        } else {
            FRAME_MS
        }
    }

    /// Redibuja fuera de la animación; si algo quedó moviéndose, arranca el temporizador.
    /// Si ya está corriendo, `tick` ajusta el intervalo en el siguiente cuadro.
    fn redraw(&mut self) {
        let motion = self.render();
        let target = timer_for(&motion, false, self.frame_ms());
        if target.is_some_and(|needed| self.timer_ms.is_none_or(|cur| needed < cur))
            || self.timer_ms.is_none()
        {
            self.set_timer(target);
        }
    }

    /// Enciende, cambia de intervalo o apaga el temporizador de animación.
    fn set_timer(&mut self, ms: Option<u32>) {
        let interval = if self.settings.high_refresh && self.hz > 60 {
            1_000_000_000 / self.hz as u64
        } else {
            FRAME_MS as u64 * 1_000_000
        };
        self.high_refresh_driver
            .interval_ns
            .store(interval, Ordering::Relaxed);
        if self.timer_ms == ms {
            return;
        }
        let needs_high_refresh =
            self.settings.high_refresh && self.hz > 60 && ms.is_some_and(|interval| interval < 15);

        unsafe {
            if needs_high_refresh {
                if self.timer_ms.is_none() {
                    self.last_tick = Instant::now();
                }
                // Con alta tasa de refresco, el hilo sincronizado con hardware VSync (DWM)
                // lleva el ritmo exacto (120/144/240 fps) sin la limitación de 10 ms de SetTimer.
                // Mantenemos además un temporizador de reserva a 30 ms por si DWM se suspende
                // durante juegos a pantalla completa, evitando cualquier congelamiento.
                SetTimer(Some(self.hwnd), ANIM_TIMER, 30, None);
                self.high_refresh_driver.set_active(true);
            } else {
                self.high_refresh_driver.set_active(false);
                match ms {
                    Some(ms) => {
                        if self.timer_ms.is_none() {
                            self.last_tick = Instant::now();
                        }
                        // Con el mismo id, SetTimer reemplaza el intervalo del temporizador existente.
                        SetTimer(Some(self.hwnd), ANIM_TIMER, ms, None);
                    }
                    None => {
                        let _ = KillTimer(Some(self.hwnd), ANIM_TIMER);
                    }
                }
            }
        }
        self.timer_ms = ms;
    }

    /// La isla se muestra si hay un aviso, o si hay música y no se ocultó sola; en ambos casos,
    /// salvo que haya algo a pantalla completa (si así está en los ajustes).
    fn update_visibility(&mut self) {
        if self.hidden_by_fullscreen() {
            // En pantalla completa (juego, video, presentación), la isla no debe aparecer
            // y si estaba visible se oculta de inmediato sin animaciones lentas que se traben.
            if let Some(backdrop) = self.backdrop.as_mut() {
                backdrop.hide();
            }
            if let Some(projector) = self.projector.as_mut() {
                projector.hide();
            }
            if self.shown {
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
                self.shown = false;
            }
            self.visible = false;
            self.expanded_by_click = false;
            self.presence.reset(0.0);
            self.high_refresh_driver.set_active(false);
            self.stop_idle_countdown();
            self.hovered = false;
            self.lingering = false;
            self.hovered_button = None;
            self.seek_hover = None;
            self.cover_hovered = false;
            self.update_hovered = false;
            self.aim_expansion(false);
            return;
        }

        let want = self.notice.is_some() || (self.media.is_some() && !self.idle_hidden);
        // Oculta por inactividad, la ventana sigue mostrada para la franja del borde.
        if self.edge_strip() && !self.shown {
            self.render();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            self.shown = true;
            self.raise();
        }
        // Ya no corresponde la franja y la isla no está: se oculta la ventana.
        if !want && !self.visible && self.shown && !self.edge_strip() {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.shown = false;
        }
        if want == self.visible {
            return;
        }
        self.visible = want;
        if want {
            if !self.shown {
                // Se dibuja ya escondida arriba antes de mostrar la ventana, para que no parpadee.
                self.render();
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                }
                self.shown = true;
            }
            // También al volver desde la franja: la ventana ya estaba mostrada, pero pudo quedar
            // debajo de otras.
            self.raise();
            self.presence.set_target(1.0);
            self.start_idle_countdown();
        } else {
            self.stop_idle_countdown();
            // Sale hacia arriba y a la vez se contrae; la ventana se oculta al terminar (en `tick`).
            self.expanded_by_click = false;
            self.presence.set_target(0.0);
            self.hovered = false;
            self.lingering = false;
            self.hovered_button = None;
            self.seek_hover = None;
            self.cover_hovered = false;
            self.update_hovered = false;
            self.aim_expansion(false);
        }
        self.start_animation();
    }

    /// Comprueba activamente si hay una app o juego en pantalla completa.
    fn poll_fullscreen(&mut self) {
        if !self.settings.hide_fullscreen {
            if self.fullscreen {
                self.set_fullscreen(false);
            }
            return;
        }
        let active = fullscreen::is_fullscreen(self.hwnd);
        if active != self.fullscreen {
            self.set_fullscreen(active);
        }
    }

    fn set_fullscreen(&mut self, fullscreen: bool) {
        if self.fullscreen == fullscreen {
            return;
        }
        self.fullscreen = fullscreen;
        self.update_visibility();
        // Los juegos y videos a pantalla completa pueden dejar la isla debajo de otras ventanas
        // aunque siga marcada como "siempre encima"; al salir de ellos se vuelve a subir.
        if !fullscreen && self.shown {
            self.raise();
        }
    }

    /// Vuelve a poner la ventana sobre las demás. `WS_EX_TOPMOST` solo se aplica al crearla:
    /// si algo la empuja hacia abajo, Windows no la devuelve sola.
    fn raise(&self) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    /// Qué hay en el punto (x, y) de la ventana; sin música no hay botones ni barra.
    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        // Durante un aviso no hay botones: un clic solo lo cierra.
        if !self.visible || self.notice.is_some() {
            return None;
        }
        let media = self.media.as_ref()?;
        island::hit_test(
            self.surface.width as f32,
            self.surface.height as f32,
            self.scale,
            self.spring.value,
            self.presence.value,
            self.shape(),
            // Barra de progreso de la canción.
            media.timeline.is_some(),
            media.sessions.1 > 1,
            self.update.is_some(),
            x,
            y,
        )
    }

    fn set_hover(&mut self, hit: Option<Hit>) {
        let button = match hit {
            Some(Hit::Button(b)) => Some(b),
            _ => None,
        };
        // Sobre la barra importa también dónde: el globo muestra ese tiempo.
        let seek = match hit {
            Some(Hit::Seek(fraction)) => Some(fraction),
            _ => None,
        };
        let cover = hit == Some(Hit::Cover);
        let update = hit == Some(Hit::Update);
        if self.hovered_button == button
            && self.seek_hover == seek
            && self.cover_hovered == cover
            && self.update_hovered == update
        {
            return;
        }
        self.hovered_button = button;
        self.seek_hover = seek;
        self.cover_hovered = cover;
        self.update_hovered = update;
        self.redraw();
    }

    /// Botón apretado sobre la barra: empieza a arrastrarla. El mouse queda capturado, así
    /// sigue llegando aunque salga de la isla.
    fn begin_drag(&mut self, fraction: f32) {
        self.dragging = Some(fraction);
        self.seek_hover = Some(fraction);
        unsafe {
            SetCapture(self.hwnd);
        }
        self.redraw();
    }

    /// El mouse se movió arrastrando la barra: muestra ese momento de la canción.
    fn drag_to(&mut self, x: f32) {
        let fraction = island::seek_fraction(
            self.surface.width as f32,
            self.surface.height as f32,
            self.scale,
            self.spring.value,
            self.presence.value,
            self.shape(),
            x,
        );
        if self.dragging != Some(fraction) {
            self.dragging = Some(fraction);
            self.seek_hover = Some(fraction);
            self.redraw();
        }
    }

    /// Se soltó el botón (`seek` = saltar ahí) o se perdió el mouse (Alt+Tab, por ejemplo:
    /// no salta).
    fn end_drag(&mut self, seek: bool) {
        let Some(fraction) = self.dragging.take() else {
            return;
        };
        unsafe {
            let _ = ReleaseCapture();
        }
        if seek {
            self.seek(fraction);
        }
        // Si se soltó fuera de la isla, Windows avisa enseguida que el mouse ya no está.
        if self.hovered {
            self.hovered = false;
            self.set_hovered(true);
        }
        self.redraw();
    }

    fn monitor_rect(&self) -> RECT {
        let monitor = monitors::find(&self.settings.monitor);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            let _ = GetMonitorInfoW(monitor, &mut info);
        }
        info.rcMonitor
    }

    /// Inicia el arrastre de la isla a lo largo del borde superior.
    fn begin_drag_island(&mut self, pending_hit: Option<Hit>) {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            SetCapture(self.hwnd);
        }
        self.drag_island = Some(DragIsland {
            start_cursor_x: pt.x,
            start_pos_x: self.pos.x,
            has_moved: false,
            pending_hit,
            last_drag: Instant::now(),
        });
    }

    /// Mueve la isla horizontalmente mientras se arrastra el mouse.
    fn drag_island_to(&mut self) {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let Some((start_cursor_x, start_pos_x, has_moved)) = self
            .drag_island
            .as_ref()
            .map(|d| (d.start_cursor_x, d.start_pos_x, d.has_moved))
        else {
            return;
        };
        let delta = pt.x - start_cursor_x;
        if !has_moved {
            if delta.abs() >= 4 {
                if let Some(drag) = self.drag_island.as_mut() {
                    drag.has_moved = true;
                    drag.last_drag = Instant::now();
                }
                self.set_hover(None);
            } else {
                return;
            }
        }
        // Limita el ritmo de actualización al configurado (60 Hz si está desactivado, 120/240 Hz si está activado).
        let min_interval = Duration::from_millis(self.frame_ms() as u64);
        if let Some(drag) = self.drag_island.as_mut() {
            if drag.last_drag.elapsed() < min_interval {
                return;
            }
            drag.last_drag = Instant::now();
        }
        let target_x = start_pos_x + delta;
        self.set_island_x(target_x);
    }

    /// Fija la posición horizontal de la isla, asegurando que quede dentro de la pantalla.
    fn set_island_x(&mut self, target_x: i32) {
        let rc = self.monitor_rect();
        let (min_x, max_x) = island_bounds(&rc, self.surface.width, self.scale);
        let clamped_x = target_x.clamp(min_x, max_x);
        if self.pos.x != clamped_x {
            let dx = clamped_x - self.pos.x;
            self.pos.x = clamped_x;
            self.x_spring.reset(clamped_x as f32);
            let center_x = rc.left + ((rc.right - rc.left) - self.surface.width as i32) / 2;
            self.settings.offset_x = ((clamped_x - center_x) as f32 / self.scale).round() as i32;
            let moved = (|| -> Result<()> {
                let mut batch = unsafe {
                    let batch = BeginDeferWindowPos(4)?;
                    DeferWindowPos(
                        batch,
                        self.hwnd,
                        None,
                        self.pos.x,
                        self.pos.y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    )?
                };
                if let Some(backdrop) = self.backdrop.as_ref() {
                    batch = backdrop.defer_move(batch, dx)?;
                }
                if let Some(projector) = self.projector.as_ref() {
                    batch = projector.defer_move(batch, dx)?;
                }
                unsafe { EndDeferWindowPos(batch) }
            })();
            if moved.is_ok() {
                if let Some(backdrop) = self.backdrop.as_mut() {
                    backdrop.moved_by(dx);
                }
                if let Some(projector) = self.projector.as_mut() {
                    projector.moved_by(dx);
                }
            } else {
                // UpdateLayeredWindow vuelve a sincronizar la posición si el lote no se acepta.
                self.redraw();
            }
        }
    }

    /// Centra la isla horizontalmente en la pantalla actual con una animación suave de resorte.
    fn center_island(&mut self) {
        let rc = self.monitor_rect();
        let center_x = rc.left + ((rc.right - rc.left) - self.surface.width as i32) / 2;
        self.settings.offset_x = 0;
        self.settings.save();
        if self.pos.x != center_x {
            self.x_spring.set_target(center_x as f32);
            self.start_animation();
        }
    }

    /// Clic en la barra: pide el salto a la app y mueve la barra ya, sin esperar su aviso.
    fn seek(&mut self, fraction: f32) {
        media::seek(fraction);
        if let Some(t) = self.media.as_mut().and_then(|m| m.timeline.as_mut()) {
            t.position = fraction * t.duration;
            t.read_at = Instant::now();
        }
        self.redraw();
    }

    fn set_media(&mut self, media: Option<NowPlaying>) {
        // SMTC repite eventos sin cambios reales; solo redibujamos si algo cambió.
        if self.media == media {
            return;
        }
        let same_text = matches!(
            (&self.media, &media),
            (Some(a), Some(b)) if a.title == b.title && a.artist == b.artist
        );
        if !same_text {
            // Canción nueva: el desplazamiento empieza otra vez desde el principio.
            self.scroll_start = Instant::now();
        }
        if media.is_some() {
            self.last_media = media.clone();
        } else {
            // Sin música no hay franja; cuando vuelva a sonar algo, la isla aparece un momento.
            self.idle_hidden = false;
        }
        self.media = media;
        self.request_lyrics();
        self.update_visibility();
        self.redraw();
    }

    fn set_hovered(&mut self, hovered: bool) {
        // Mientras sale de la pantalla no responde al mouse.
        if self.hovered == hovered || (hovered && !self.visible) {
            return;
        }
        self.hovered = hovered;
        // Al salir sigue expandida un momento (`COLLAPSE_TIMER`); al volver se cancela.
        self.lingering =
            !hovered && self.visible && (self.expanded_by_click || self.settings.expand_on_hover);
        unsafe {
            if self.lingering {
                SetTimer(Some(self.hwnd), COLLAPSE_TIMER, COLLAPSE_DELAY_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), COLLAPSE_TIMER);
            }
        }
        if hovered {
            self.stop_idle_countdown();
            // Pide a Windows un aviso (WM_MOUSELEAVE) cuando el mouse salga de la isla.
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            self.scroll_start = Instant::now();
        } else if self.notice.is_none() {
            self.start_idle_countdown();
        }
        self.update_expansion();
    }

    /// Terminó la espera tras salir el mouse: ahora sí se encoge.
    fn end_linger(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), COLLAPSE_TIMER);
        }
        self.lingering = false;
        self.expanded_by_click = false;
        self.update_expansion();
    }

    fn start_animation(&mut self) {
        self.set_timer(Some(self.frame_ms()));
    }

    fn tick(&mut self) {
        if self.hidden_by_fullscreen() {
            if let Some(projector) = self.projector.as_mut() {
                projector.hide();
            }
            if self.shown {
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
                self.shown = false;
            }
            self.set_timer(None);
            return;
        }

        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;

        let centering = self.x_spring.step(dt);
        let new_x = self.x_spring.value.round() as i32;
        if self.pos.x != new_x {
            self.pos.x = new_x;
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    self.pos.x,
                    self.pos.y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }

        let expanding = self.spring.step(dt);
        let entering = self.presence.step(dt);
        let lyrics_moving = self.lyrics_spring.step(dt);
        let reshaping = self.notice_spring.step(dt);
        let moving = expanding || entering || lyrics_moving || reshaping || centering;
        let motion = self.render();

        // Terminó de salir: ahora sí se oculta la ventana (salvo que deba quedar la franja).
        if !self.visible && !entering && self.shown && !self.edge_strip() {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.shown = false;
        }

        // El temporizador sigue solo mientras algo se mueve, y al ritmo que eso necesita.
        let frame_ms = self.frame_ms();
        self.set_timer(timer_for(&motion, moving, frame_ms));
    }

    /// Forma de la isla expandida para `island`: (ancho con letra, forma de aviso).
    fn shape(&self) -> (f32, f32) {
        (self.lyrics_spring.value, self.notice_spring.value)
    }

    /// Dibuja el cuadro actual y dice qué sigue moviéndose.
    fn render(&mut self) -> Motion {
        // Si la música acaba de terminar, la isla sale mostrando la última canción, no vacía.
        let media = self.media.as_ref().or(self.last_media.as_ref());
        let cover = media.and_then(|m| m.cover.as_ref());
        let now = Instant::now();
        let volume = self.volume_until.and_then(|until| {
            if now < until {
                let remaining = until.saturating_duration_since(now).as_secs_f32();
                let fade = (remaining / 0.25).clamp(0.0, 1.0);
                Some((self.volume_level, fade))
            } else {
                None
            }
        });
        let pressed = self.pressed.and_then(|(button, at)| {
            let t = at.elapsed().as_secs_f32() / island::PRESS_SECS;
            (t < 1.0).then_some((button, t))
        });
        if pressed.is_none() {
            self.pressed = None;
        }
        // El aviso que terminó se sigue viendo mientras la isla se contrae (o sale), hasta que
        // su contenido se desvanece; si el mouse está encima, vuelve la música.
        if self.notice.is_none() && (self.hovered || self.spring.value <= 0.55) {
            self.last_notice = None;
        }
        let notice = self.notice.as_ref().or(self.last_notice.as_ref());
        // Con un aviso la isla toma su forma de una fila; al terminar vuelve a la de la música.
        let notice_shape = if notice.is_some() { 1.0 } else { 0.0 };
        self.notice_spring.set_target(notice_shape);
        let reshaping = self.notice_spring.value != notice_shape;
        let strip = self.edge_strip();
        let playing = (self.visible || strip) && self.media.as_ref().is_some_and(|m| m.playing);
        let bars = self.visible && playing && island::mini_alpha(self.spring.value) > 0.0;
        // Se escucha el audio de la app solo mientras se ven las barras.
        let listen = self
            .media
            .as_ref()
            .filter(|_| bars && self.settings.real_bars)
            .map(|m| m.app_id.as_str());
        self.visualizer.set_target(listen);
        let levels = self.visualizer.levels();
        let controls = media.map(|m| Controls {
            levels,
            pressed,
            playing: m.playing,
            hovered: self.hovered_button,
            seek_hover: self
                .seek_hover
                .zip(m.timeline)
                .map(|(fraction, t)| (fraction, clock(fraction * t.duration))),
            accent: cover.map_or([255, 255, 255], |c| c.accent),
            progress: m
                .timeline
                .map(|t| self.dragging.unwrap_or_else(|| t.progress(m.playing))),
            volume,
            sessions: m.sessions,
            cover_hovered: self.cover_hovered,
            update: self.update.is_some(),
            update_hovered: self.update_hovered,
            lyrics_active: self.settings.lyrics_island,
        });
        // El contenido expandido (y con él la barra) se ve a partir de ~55 % de la expansión.
        // Mientras se muestra el volumen también hace falta redibujar: para quitarlo a tiempo.
        let progress = (self.visible
            && playing
            && self.spring.value > 0.55
            && self.media.as_ref().is_some_and(|m| m.timeline.is_some()))
            || volume.is_some();
        // Ancho del texto del aviso, para centrar ícono y texto en la isla.
        let notice_text_w = match (notice, self.text.as_mut()) {
            (Some(n), Some(text)) => text.measure(&n.title, &n.subtitle),
            _ => 0.0,
        };
        let time_info = media.and_then(|m| m.timeline).map(|t| {
            // Arrastrando la barra, los tiempos y la letra muestran el momento elegido.
            let current = self
                .dragging
                .map_or_else(|| t.current_position(playing), |f| f * t.duration);
            island::TimeInfo {
                current: clock(current),
                remaining: format!("-{}", clock(t.duration - current)),
            }
        });
        let mut lyric_painting = false;
        let lyrics_info = media.map(|m| {
            let key = lyrics::key(&m.title, &m.artist);
            let mut info = island::LyricsInfo {
                lines: Vec::new(),
                shift: 0.0,
                sung: None,
                status: None,
            };
            if !self.settings.lyrics_island && self.lyrics_spring.value <= 0.01 {
                // Apagada y ya desvanecida: nada que dibujar.
            } else if let Some(lines) = self.lyrics.get(&key) {
                if lines.is_empty() {
                    if self.settings.lyrics_island {
                        info.status = Some("Sin letra sincronizada".to_string());
                    }
                } else if let Some(t) = m.timeline {
                    let secs = self
                        .dragging
                        .map_or_else(|| t.current_position(m.playing), |f| f * t.duration);
                    // Verso que se canta; -1 = aún no empieza: el primero espera abajo.
                    let current = lyrics::index_at(lines, secs);
                    let index = current.map_or(-1, |i| i as isize);
                    info.shift = slide_shift(&mut self.lyric_slide, &key, index);
                    // Dos de más arriba y abajo: entran y salen durante la subida.
                    // La intro (antes del primer verso) cuenta como un verso vacío: ahí va la
                    // nota musical, y sube como los demás cuando empieza la letra.
                    info.lines = (-2..=3)
                        .filter_map(|offset: isize| {
                            let text = match index + offset {
                                -1 => String::new(),
                                i => lines.get(usize::try_from(i).ok()?)?.text.clone(),
                            };
                            Some((offset as i32, text))
                        })
                        .collect();
                    // El verso que se canta se va pintando al ritmo de la voz.
                    if let Some(i) = current {
                        let (pos, len) = lyrics::sung(lines, i, secs);
                        info.sung = Some(pos);
                        lyric_painting = m.playing && pos < len;
                    }
                }
            } else if self.settings.lyrics_island {
                let status = if self.lyrics_pending.contains(&key) {
                    "Buscando letra..."
                } else {
                    "Sin letra sincronizada"
                };
                info.status = Some(status.to_string());
            }
            info
        });
        let shape = self.shape();
        // Solo se trabaja la zona donde se dibuja ahora y donde se dibujó antes (para borrarlo):
        // con la isla contraída es una parte mínima de la ventana.
        let (sw, sh) = (self.surface.width, self.surface.height);
        let mut drawn = Area::new(
            island::bounds(
                sw as f32,
                sh as f32,
                self.scale,
                self.spring.value,
                self.presence.value,
                shape,
            ),
            sw,
            sh,
        );
        if strip {
            drawn = drawn.union(Area::new(
                island::edge_strip_bounds(sw as f32, self.scale),
                sw,
                sh,
            ));
        }
        let dirty = match self.surface.drawn {
            Some(previous) => previous.union(drawn),
            None => self.surface.full(),
        };
        self.surface.clear(dirty);
        let blur_active = if self.settings.translucent
            && !self.hidden_by_fullscreen()
            && self.presence.value > 0.01
        {
            let (x, y, w, h, r) = island::backdrop_geometry(
                sw as f32,
                sh as f32,
                self.scale,
                self.spring.value,
                self.presence.value,
                shape,
            );
            self.backdrop.as_mut().is_some_and(|backdrop| {
                backdrop.show(
                    self.hwnd,
                    (self.pos.x, self.pos.y, sw, sh),
                    (x, y, w, h, r),
                    (self.settings.blur_radius as f32 * self.scale).round() as u32,
                    self.presence.value,
                )
            })
        } else {
            if let Some(backdrop) = self.backdrop.as_mut() {
                backdrop.hide();
            }
            false
        };
        let areas = island::draw(
            &mut self.surface.pixmap,
            self.scale,
            self.spring.value,
            self.presence.value,
            shape,
            controls,
            notice.map(|n| (&n.view, notice_text_w)),
            cover.map(|c| c.image.as_ref()),
            media
                .and_then(|m| m.app_icon.as_ref())
                .map(|i| i.0.as_ref()),
            self.started.elapsed().as_secs_f32(),
            time_info,
            lyrics_info,
            blur_active,
        );
        let lyrics_area = areas.as_ref().and_then(|a| a.lyrics.as_ref());
        let lyric_sliding = lyrics_area.is_some_and(|l| l.shift != 0.0);
        // Solo se repinta si la letra se ve.
        let lyric_painting = lyric_painting && lyrics_area.is_some();
        let mut edge_ms = None;
        if let (true, Some(m)) = (strip, self.media.as_ref()) {
            let accent = m.cover.as_ref().map_or([255, 255, 255], |c| c.accent);
            let progress = m.timeline.map(|t| t.progress(m.playing));
            island::draw_edge_strip(
                &mut self.surface.pixmap,
                self.scale,
                progress,
                accent,
                self.update.is_some(),
            );
            // Se redibuja solo cuando la línea crece un píxel (en pausa, nunca).
            if let (true, Some(t)) = (m.playing, m.timeline) {
                let px = island::edge_strip_width(self.scale).max(1.0);
                let ms = (t.duration * 1000.0 / px) as u32;
                edge_ms = Some(ms.clamp(PROGRESS_MS, 5000));
            }
        }
        self.surface.copy_area(dirty);
        self.surface.drawn = Some(drawn);

        // Proyector de la letra debajo de la isla (solo si la isla está contraída u oculta en el borde):
        let mut lyric_ms = None;
        let mut project_sung = None;
        let mut project_painting = false;
        let should_project = !self.hidden_by_fullscreen()
            && self.spring.value < 0.4
            && self.settings.lyrics_projector
            && playing;
        let frame_ms = self.frame_ms();
        let lyric = if should_project {
            self.media.as_ref().and_then(|m| {
                let lines = self.lyrics.get(&lyrics::key(&m.title, &m.artist))?;
                let t = m.timeline?;
                let secs = t.current_position(true);
                lyric_ms = lyrics::next_start(lines, secs)
                    .map(|next| (((next - secs) * 1000.0) as u32).clamp(frame_ms, 5000));
                let current = lyrics::index_at(lines, secs);
                if let Some(i) = current {
                    let (pos, len) = lyrics::sung(lines, i, secs);
                    project_sung = Some(pos);
                    project_painting = m.playing && pos < len;
                }
                lyrics::line_at(lines, secs)
            })
        } else if self.visible && playing && self.settings.lyrics_island {
            if let Some(m) = &self.media {
                if let (Some(lines), Some(t)) = (
                    self.lyrics.get(&lyrics::key(&m.title, &m.artist)),
                    m.timeline,
                ) {
                    let secs = t.current_position(true);
                    // Despierta al comenzar el próximo verso, también con la letra dentro de la isla.
                    lyric_ms = lyrics::next_start(lines, secs)
                        .map(|next| (((next - secs) * 1000.0) as u32).clamp(frame_ms, 5000));
                }
            }
            None
        } else {
            None
        };
        let mut projecting = false;
        if lyric.is_some() && self.projector.is_none() {
            self.projector = Projector::new(self.scale).ok();
        }
        let hide_projector = self.hidden_by_fullscreen();
        if let Some(projector) = self.projector.as_mut() {
            let island_bottom = island::bottom(
                self.surface.width as f32,
                self.surface.height as f32,
                self.scale,
                self.spring.value,
                self.presence.value,
                shape,
            );
            // Oculta en el borde, el proyector queda bajo la línea de tiempo.
            let bottom = island_bottom.max(4.0 * self.scale);
            let cx = self.pos.x + self.surface.width as i32 / 2;
            let top = self.pos.y + (bottom - 2.0 * self.scale) as i32;
            if hide_projector {
                projector.hide();
            } else {
                projecting = projector.render(
                    lyric,
                    project_sung,
                    cx,
                    top,
                    self.settings.translucent,
                    self.settings.blur_radius,
                );
            }
        }

        let mut scroll_ms = None;
        if let (Some(areas), Some(text)) = (areas, self.text.as_mut()) {
            let (title, subtitle) = match (notice, media) {
                (Some(n), _) => (n.title.clone(), n.subtitle.clone()),
                (None, Some(m)) => {
                    let title = if m.title.is_empty() {
                        "Sin título"
                    } else {
                        &m.title
                    };
                    let mut subtitle = m.artist.clone();
                    if let Some((level, _)) = volume {
                        let kind = if self.volume_app { "app" } else { "general" };
                        let percent = (level * 100.0).round() as u32;
                        subtitle = format!("Volumen {kind} {percent} %");
                    } else if let (true, Some(u)) = (self.update_hovered, &self.update) {
                        // Explica qué hace el punto azul antes de hacer clic.
                        subtitle = u.menu_label();
                    } else if !m.playing {
                        if !subtitle.is_empty() {
                            subtitle.push_str(" · ");
                        }
                        subtitle.push_str("En pausa");
                    }
                    (title.to_string(), subtitle)
                }
                (None, None) => (
                    "Nada en reproducción".to_string(),
                    "Pon música en cualquier app".to_string(),
                ),
            };
            scroll_ms = text.draw(
                self.surface.dib(),
                sw,
                sh,
                (dirty.x0, dirty.y0, dirty.x1, dirty.y1),
                &areas,
                &title,
                &subtitle,
                self.scroll_start.elapsed().as_secs_f32(),
            );
        }

        let size = SIZE {
            cx: self.surface.width as i32,
            cy: self.surface.height as i32,
        };
        let src = POINT::default();
        // Opacidad global: la isla se desvanece mientras entra o sale de la pantalla.
        // Con la línea del borde, opaca: la isla sube sin desvanecerse y la línea queda visible.
        let opacity = if strip {
            255
        } else {
            (self.presence.value.clamp(0.0, 1.0) * 255.0) as u8
        };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: opacity,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // Windows solo vuelve a componer la zona que cambió.
        let rect = RECT {
            left: dirty.x0 as i32,
            top: dirty.y0 as i32,
            right: dirty.x1 as i32,
            bottom: dirty.y1 as i32,
        };
        let info = UPDATELAYEREDWINDOWINFO {
            cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            hdcDst: HDC::default(),
            pptDst: &self.pos,
            psize: &size,
            hdcSrc: self.surface.dc,
            pptSrc: &src,
            crKey: COLORREF(0),
            pblend: &blend,
            dwFlags: ULW_ALPHA,
            prcDirty: if dirty.is_empty() {
                std::ptr::null()
            } else {
                &rect
            },
        };
        unsafe {
            // Los píxeles 100 % transparentes dejan pasar los clics a lo que hay debajo.
            if !UpdateLayeredWindowIndirect(self.hwnd, &info).as_bool() {
                // Si Windows no acepta la zona, la ventana entera.
                let _ = UpdateLayeredWindow(
                    self.hwnd,
                    None,
                    Some(&self.pos),
                    Some(&size),
                    Some(self.surface.dc),
                    Some(&src),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
            }
        }
        if let Some(backdrop) = self.backdrop.as_mut() {
            backdrop.place_behind(self.hwnd);
        }
        Motion {
            scroll_ms,
            bars,
            progress,
            edge_ms,
            // El último cuadro de la animación se dibuja ya con el botón en reposo.
            pressing: pressed.is_some(),
            projecting,
            lyric_ms,
            lyric_sliding,
            lyric_painting: lyric_painting || project_painting,
            reshaping,
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.high_refresh_driver.set_active(false);
    }
}

/// Intervalo del temporizador según lo que se mueve: 60 fps (o más si el monitor lo soporta) para la isla,
/// un cuadro por píxel de avance de un texto largo (y ninguno durante sus pausas), 30 fps si solo bailan
/// las barras, 20 fps si se pinta el verso, 4 fps si solo avanza la barra de progreso, un cuadro por píxel
/// de avance si la isla está oculta y solo crece la línea del borde, y apagado si todo está quieto.
/// Con letra, además, a más tardar cuando empieza el verso siguiente.
fn timer_for(motion: &Motion, moving: bool, frame_ms: u32) -> Option<u32> {
    let base = if moving
        || motion.pressing
        || motion.projecting
        || motion.lyric_sliding
        || motion.reshaping
    {
        Some(frame_ms)
    } else if motion.bars {
        Some(BARS_MS)
    } else if motion.lyric_painting {
        Some(PAINT_MS)
    } else if motion.progress {
        Some(PROGRESS_MS)
    } else {
        motion.edge_ms
    };
    [base, motion.lyric_ms, motion.scroll_ms]
        .into_iter()
        .flatten()
        .min()
}

/// Límites mínimo y máximo de `pos.x` para que la isla quepa dentro de la pantalla.
fn island_bounds(rc: &RECT, width: u32, scale: f32) -> (i32, i32) {
    let half_w = width as i32 / 2;
    let margin = (16.0 * scale).round() as i32;
    let max_island_w = (island::EXPANDED_LYRICS.0 * scale).round() as i32;
    let half_island = max_island_w / 2 + margin;
    let min_cx = rc.left + half_island;
    let max_cx = rc.right - half_island;
    if min_cx > max_cx {
        let center_cx = rc.left + (rc.right - rc.left) / 2;
        (center_cx - half_w, center_cx - half_w)
    } else {
        (min_cx - half_w, max_cx - half_w)
    }
}

/// Calcula escala, superficie de dibujo y posición (arriba al centro, con desplazamiento opcional) para un monitor.
fn layout_for(monitor: HMONITOR, offset_x: i32) -> Result<(Surface, POINT, f32)> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
    unsafe {
        let _ = GetMonitorInfoW(monitor, &mut info);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    }
    let scale = dpi_x as f32 / 96.0;

    let (lw, lh) = island::window_size();
    let width = (lw * scale).ceil() as u32;
    let height = (lh * scale).ceil() as u32;

    let rc: RECT = info.rcMonitor;
    let (min_x, max_x) = island_bounds(&rc, width, scale);
    let center_x = rc.left + ((rc.right - rc.left) - width as i32) / 2;
    let target_x = center_x + (offset_x as f32 * scale).round() as i32;
    let pos = POINT {
        x: target_x.clamp(min_x, max_x),
        y: rc.top,
    };
    Ok((Surface::new(width, height)?, pos, scale))
}

/// Mapa de bits en memoria que Windows usa para pintar la ventana layered.
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    width: u32,
    height: u32,
    pixmap: Pixmap,
    /// Zona dibujada en el cuadro anterior (`None` = todavía ninguno: el próximo es completo).
    drawn: Option<Area>,
}

/// Rectángulo de píxeles [x0, x1) x [y0, y1) dentro de una superficie; puede estar vacío.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Area {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Area {
    /// Redondea hacia afuera (x0, y0, x1, y1) y lo recorta a una superficie de `w` x `h`.
    fn new((x0, y0, x1, y1): (f32, f32, f32, f32), w: u32, h: u32) -> Self {
        let clip = |v: f32, max: u32| (v.max(0.0) as u32).min(max);
        let area = Self {
            x0: clip(x0.floor(), w),
            y0: clip(y0.floor(), h),
            x1: clip(x1.ceil(), w),
            y1: clip(y1.ceil(), h),
        };
        if area.is_empty() {
            Self::EMPTY
        } else {
            area
        }
    }

    const EMPTY: Self = Self {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    };

    fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    /// El menor rectángulo que cubre a los dos.
    fn union(self, other: Self) -> Self {
        match (self.is_empty(), other.is_empty()) {
            (true, _) => other,
            (_, true) => self,
            _ => Self {
                x0: self.x0.min(other.x0),
                y0: self.y0.min(other.y0),
                x1: self.x1.max(other.x1),
                y1: self.y1.max(other.y1),
            },
        }
    }

    #[cfg(test)]
    fn pixels(&self) -> u32 {
        if self.is_empty() {
            0
        } else {
            (self.x1 - self.x0) * (self.y1 - self.y0)
        }
    }
}

impl Surface {
    fn new(width: u32, height: u32) -> Result<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32), // negativo = filas de arriba hacia abajo
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            {
                Ok(b) => b,
                Err(e) => {
                    let _ = DeleteDC(dc);
                    return Err(e);
                }
            };
            let old = SelectObject(dc, bitmap.into());
            let pixmap = Pixmap::new(width, height).expect("tamaño de ventana inválido");
            Ok(Self {
                dc,
                bitmap,
                old,
                bits: bits as *mut u8,
                width,
                height,
                pixmap,
                drawn: None,
            })
        }
    }

    fn full(&self) -> Area {
        Area {
            x0: 0,
            y0: 0,
            x1: self.width,
            y1: self.height,
        }
    }

    /// tiny-skia guarda RGBA premultiplicado; Windows espera BGRA premultiplicado.
    fn copy_to_dib(&mut self) {
        self.copy_area(self.full());
    }

    /// Deja transparente la zona `area` del dibujo.
    fn clear(&mut self, area: Area) {
        if area.is_empty() {
            return;
        }
        let stride = self.width as usize * 4;
        let data = self.pixmap.data_mut();
        for y in area.y0 as usize..area.y1 as usize {
            data[y * stride + area.x0 as usize * 4..y * stride + area.x1 as usize * 4].fill(0);
        }
    }

    /// Los píxeles del DIB (BGRA premultiplicado, fila tras fila), lo que muestra Windows.
    fn dib(&mut self) -> &mut [u8] {
        let len = self.width as usize * 4 * self.height as usize;
        unsafe { std::slice::from_raw_parts_mut(self.bits, len) }
    }

    /// Como `copy_to_dib`, solo la zona `area`.
    fn copy_area(&mut self, area: Area) {
        if area.is_empty() {
            return;
        }
        let stride = self.width as usize * 4;
        let len = stride * self.height as usize;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, len) };
        let src = self.pixmap.data();
        for y in area.y0 as usize..area.y1 as usize {
            let row = y * stride + area.x0 as usize * 4..y * stride + area.x1 as usize * 4;
            for (d, s) in dst[row.clone()]
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(src[row].as_chunks::<4>().0)
            {
                *d = [s[2], s[1], s[0], s[3]];
            }
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Instala la versión nueva (primero avisa "Instalando" y luego se cierra, ver
/// `begin_install`) o, si no está descargada, abre la página de descarga.
fn apply_update(update: Option<&update::Update>) {
    let Some(update) = update else { return };
    if update.installer.is_some() {
        with_app(|app| app.begin_install(update.clone()));
    } else {
        update.apply();
    }
}

/// Pasó la espera tras el aviso "Instalando": se lanza la instalación y se cierra la isla.
/// Va fuera de `with_app`: cerrar la ventana necesita el estado de la app libre.
fn finish_install(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(Some(hwnd), INSTALL_TIMER);
    }
    let mut update = None;
    with_app(|app| update = app.installing.take());
    let Some(update) = update else { return };
    if update.apply() {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    } else {
        // No arrancó: `apply` abrió la página de descarga.
        with_app(|app| {
            app.announce(
                Notice {
                    icon: NoticeIcon::Failed,
                    color: RED,
                },
                "No se pudo iniciar la instalación",
                "Se abrió la página de descarga",
                NOTICE_UPDATE_MS,
            )
        });
    }
}

/// Menú contextual sobre la isla: opciones de salida de audio y centrar en la pantalla.
/// Va fuera de `with_app`: el menú corre su propio bucle de mensajes.
fn open_island_menu(app_id: Option<&str>, is_centered: bool) {
    let (label, pids, outputs, unavailable) = if let Some(app_id) = app_id {
        let label = audio::app_label(app_id);
        let pids = audio::app_pids(app_id);
        let available = routing::available();
        let outputs = if available {
            routing::outputs()
        } else {
            Vec::new()
        };
        let unavailable = if !available {
            Some("No disponible en esta versión de Windows")
        } else if pids.is_empty() {
            Some("No se encontró el proceso de la app")
        } else if outputs.is_empty() {
            Some("No hay salidas de audio")
        } else {
            None
        };
        (Some(label), pids, outputs, unavailable)
    } else {
        (None, Vec::new(), Vec::new(), None)
    };
    let current = if unavailable.is_none() && !pids.is_empty() {
        routing::current(&pids)
    } else {
        None
    };
    let Some(choice) = tray::show_island_menu(
        label.as_deref(),
        &outputs,
        current.as_deref(),
        unavailable,
        is_centered,
    ) else {
        return;
    };
    match choice {
        tray::IslandMenuChoice::Center => {
            with_app(|app| app.center_island());
        }
        tray::IslandMenuChoice::Output(choice) => {
            let Some(label) = label else {
                return;
            };
            let device = choice.and_then(|i| outputs.get(i));
            let ok = routing::set(&pids, device.map(|o| o.id.as_str()));
            let subtitle = match (ok, device) {
                (false, _) => "No se pudo cambiar".to_string(),
                (true, Some(o)) => o.name.clone(),
                (true, None) => "Predeterminada del sistema".to_string(),
            };
            with_app(|app| {
                let view = Notice {
                    icon: NoticeIcon::Audio,
                    color: BLUE,
                };
                app.show_notice(view, &format!("Salida de {label}"), &subtitle, NOTICE_MS);
            });
        }
    }
}

/// Coordenadas del mouse en la ventana (con signo, como GET_X_LPARAM / GET_Y_LPARAM).
fn mouse_pos(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as u16 as i16;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16;
    (x as f32, y as f32)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let deferrable = msg >= WM_APP
        || matches!(
            msg,
            WM_MOUSEMOVE
                | WM_MOUSELEAVE
                | WM_LBUTTONDOWN
                | WM_LBUTTONUP
                | WM_RBUTTONUP
                | WM_MOUSEWHEEL
                | WM_CAPTURECHANGED
                | WM_TIMER
                | WM_HOTKEY
        );
    if deferrable && APP.with(|app| app.try_borrow_mut().is_err()) {
        ui::defer(wndproc, hwnd, msg, wparam, lparam);
        return LRESULT(0);
    }
    match msg {
        WM_MOUSEMOVE => {
            let (x, y) = mouse_pos(lparam);
            with_app(|app| {
                if app.drag_island.is_some() {
                    app.drag_island_to();
                    return;
                }
                // Oculta: el mouse está en la franja del borde (o sobre la isla mientras sube).
                if app.idle_hidden {
                    app.reveal();
                    return;
                }
                if app.dragging.is_some() {
                    app.drag_to(x);
                    return;
                }
                app.set_hovered(true);
                let hit = app.hit_at(x, y);
                app.set_hover(hit);
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_app(|app| {
                // Arrastrando la barra o la isla, sigue abierta aunque el mouse salga.
                if app.dragging.is_none() && app.drag_island.is_none() {
                    app.set_hovered(false);
                    app.set_hover(None);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = mouse_pos(lparam);
            with_app(|app| {
                let hit = app.hit_at(x, y);
                if let (None, Some(Hit::Seek(fraction))) = (&app.notice, hit) {
                    app.begin_drag(fraction);
                } else {
                    app.begin_drag_island(hit);
                }
            });
            LRESULT(0)
        }
        // Otra ventana se quedó con el mouse a mitad del arrastre: se cancela sin saltar.
        WM_CAPTURECHANGED => {
            with_app(|app| {
                app.end_drag(false);
                if let Some(drag) = app.drag_island.take() {
                    if drag.has_moved {
                        app.settings.save();
                    }
                }
            });
            LRESULT(0)
        }
        // Llega a la isla aunque no tenga el foco gracias a la opción de Windows
        // "Desplazar ventanas inactivas al mantener el puntero sobre ellas" (activa por defecto).
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
            // Con Ctrl apretado, el volumen general (MK_CONTROL en la parte baja de wparam).
            let general = wparam.0 & 0x0008 != 0;
            with_app(|app| app.wheel(delta, general));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = mouse_pos(lparam);
            // La actualización se aplica fuera de `with_app`: puede cerrar la ventana, y su
            // WM_DESTROY necesita el estado de la app libre.
            let mut pending = None;
            with_app(|app| {
                if let Some(drag) = app.drag_island.take() {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    if drag.has_moved {
                        let mut pt = POINT::default();
                        unsafe {
                            let _ = GetCursorPos(&mut pt);
                        }
                        let delta = pt.x - drag.start_cursor_x;
                        app.set_island_x(drag.start_pos_x + delta);
                        app.settings.save();
                        return;
                    }
                    if app.notice.is_some() {
                        app.end_notice();
                        return;
                    }
                    // El clic que abre los controles no debe activar la carátula o un botón
                    // que acaba de aparecer bajo el puntero.
                    if !app.settings.expand_on_hover && !app.expanded_by_click {
                        app.expanded_by_click = true;
                        app.set_hovered(true);
                        app.update_expansion();
                        return;
                    }
                    match drag.pending_hit {
                        Some(Hit::Button(button)) => app.press(button),
                        Some(Hit::Seek(fraction)) => app.seek(fraction),
                        Some(Hit::Cover) => media::next_session(),
                        Some(Hit::Update) => pending = app.update.clone(),
                        None => {}
                    }
                    return;
                }
                if app.dragging.is_some() {
                    app.end_drag(true);
                    return;
                }
                if app.notice.is_some() {
                    app.end_notice();
                    return;
                }
                match app.hit_at(x, y) {
                    Some(Hit::Button(button)) => app.press(button),
                    Some(Hit::Seek(fraction)) => app.seek(fraction),
                    Some(Hit::Cover) => media::next_session(),
                    Some(Hit::Update) => pending = app.update.clone(),
                    None => {}
                }
            });
            if pending.is_some() {
                apply_update(pending.as_ref());
            }
            LRESULT(0)
        }
        // Clic derecho sobre la isla: opciones de salida de audio y centrar en pantalla.
        WM_RBUTTONUP => {
            let mut app_id = None;
            let mut is_centered = true;
            with_app(|app| {
                if (app.visible || app.edge_strip()) && app.notice.is_none() {
                    app_id = app.media.as_ref().map(|m| m.app_id.clone());
                    is_centered = app.settings.offset_x == 0;
                }
            });
            open_island_menu(app_id.as_deref(), is_centered);
            LRESULT(0)
        }
        WM_ANIM_TICK => {
            with_app(|app| {
                if app.settings.high_refresh {
                    app.tick();
                    app.last_vsync = Some(Instant::now());
                }
                app.high_refresh_driver.frame_processed();
            });
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == ANIM_TIMER => {
            with_app(|app| {
                let vsync_active = app.settings.high_refresh
                    && app.hz > 60
                    && app.timer_ms.is_some_and(|interval| interval < 15);
                if vsync_active
                    && app
                        .last_vsync
                        .is_some_and(|last| last.elapsed() < Duration::from_millis(30))
                {
                    return;
                }
                app.tick();
                app.high_refresh_driver.frame_processed();
            });
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == IDLE_TIMER => {
            with_app(|app| app.idle_hide());
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == NOTICE_TIMER => {
            with_app(|app| app.end_notice());
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == COUNTDOWN_TIMER => {
            with_app(|app| app.countdown_done());
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == INSTALL_TIMER => {
            finish_install(hwnd);
            LRESULT(0)
        }
        system::WM_AUDIO_DEVICE => {
            with_app(|app| app.on_audio_device());
            LRESULT(0)
        }
        WM_HOTKEY if hotkey::is_ours(wparam) => {
            with_app(|app| app.toggle_hidden());
            LRESULT(0)
        }
        hotkey::WM_HOTKEY_CAPTURED => {
            let captured = (lparam.0 != 0).then_some(hotkey::Hotkey {
                mods: wparam.0 as u32,
                vk: lparam.0 as u32,
            });
            with_app(|app| app.end_hotkey_capture(captured));
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == COLLAPSE_TIMER => {
            with_app(|app| app.end_linger());
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == CAPTURE_TIMER => {
            with_app(|app| app.end_hotkey_capture(None));
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == FULLSCREEN_TIMER => {
            with_app(|app| app.poll_fullscreen());
            LRESULT(0)
        }
        media::WM_MEDIA => {
            let info = unsafe { media::take(lparam) };
            with_app(|app| app.set_media(info));
            LRESULT(0)
        }
        WM_DPICHANGED | WM_DISPLAYCHANGE => {
            with_app(|app| app.relayout());
            LRESULT(0)
        }
        lyrics::WM_LYRICS => {
            let found = unsafe { lyrics::take(lparam) };
            with_app(|app| app.set_lyrics(found));
            LRESULT(0)
        }
        update::WM_UPDATE => {
            let found = unsafe { update::take(lparam) };
            // wparam 1 = la pidió "Buscar actualizaciones": se avisa en la isla.
            let manual = wparam.0 == 1;
            with_app(|app| {
                let version = &found.version;
                if manual {
                    let subtitle = if found.installer.is_some() {
                        "Haz clic en el punto azul para instalarla"
                    } else {
                        "Descárgala desde el menú de la bandeja"
                    };
                    app.announce(
                        Notice {
                            icon: NoticeIcon::Installing,
                            color: BLUE,
                        },
                        &format!("Versión {version} disponible"),
                        subtitle,
                        NOTICE_UPDATE_MS,
                    );
                } else if found.installer.is_some() {
                    app.tray.notify(
                        &format!("Notch Isla {version} lista para instalar"),
                        "Haz clic aquí para actualizar. La isla se cierra y vuelve en unos segundos.",
                    );
                } else {
                    app.tray.notify(
                        &format!("Notch Isla {version} disponible"),
                        "Haz clic aquí para descargarla, o búscala en el menú del ícono.",
                    );
                }
                app.update = Some(found);
                // Aparece el punto azul en la isla.
                app.redraw();
            });
            LRESULT(0)
        }
        update::WM_UPDATE_CHECKED => {
            let failed = wparam.0 == 1;
            with_app(|app| app.update_checked(failed));
            LRESULT(0)
        }
        // Clic derecho sobre el ícono de la bandeja: menú. El evento real va en la parte baja de lparam.
        tray::WM_TRAY => {
            let event = (lparam.0 & 0xFFFF) as u32;
            // Se copia antes de actuar: mientras el menú está abierto no debe haber préstamo de la app.
            let mut pending = None;
            let mut settings = None;
            let mut timer_left = None;
            with_app(|app| {
                pending = app.update.clone();
                settings = Some(app.settings.clone());
                timer_left = app.countdown_left();
            });
            if event == NIN_BALLOONUSERCLICK {
                apply_update(pending.as_ref());
            } else if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                let Some(settings) = settings else {
                    return LRESULT(0);
                };
                let label = pending.as_ref().map(update::Update::menu_label);
                let monitors = monitors::list();
                let state = tray::MenuState {
                    update: label.as_deref(),
                    settings,
                    monitors: &monitors,
                    timer_left,
                };
                use tray::Command;
                tray::show_menu(state, |cmd, settings| match cmd {
                    Command::ToggleAutostart => {
                        autostart::set(!autostart::is_enabled());
                        true
                    }
                    Command::Update => {
                        apply_update(pending.as_ref());
                        false
                    }
                    Command::Exit => {
                        unsafe {
                            let _ = DestroyWindow(hwnd);
                        }
                        false
                    }
                    Command::Timer(minutes) => {
                        with_app(|app| app.start_countdown(minutes));
                        false
                    }
                    Command::CancelTimer => {
                        with_app(|app| app.cancel_countdown());
                        false
                    }
                    Command::ChangeHotkey => {
                        with_app(|app| app.begin_hotkey_capture());
                        false
                    }
                    Command::CheckUpdates => {
                        with_app(|app| app.check_updates_now());
                        false
                    }
                    Command::Center => {
                        with_app(|app| app.center_island());
                        settings.offset_x = 0;
                        true
                    }
                    Command::HideDelay(secs) => {
                        settings.hide_delay = secs;
                        settings.save();
                        with_app(|app| app.apply_settings(settings.clone()));
                        true
                    }
                    Command::Monitor(i) => {
                        settings.monitor = monitors[i].device.clone();
                        settings.save();
                        with_app(|app| app.apply_settings(settings.clone()));
                        true
                    }
                    Command::ToggleNotices => {
                        settings.notices = !settings.notices;
                        settings.save();
                        with_app(|app| app.apply_settings(settings.clone()));
                        true
                    }
                    Command::ToggleLyricsIsland => {
                        settings.lyrics_island = !settings.lyrics_island;
                        settings.save();
                        with_app(|app| app.apply_settings(settings.clone()));
                        true
                    }
                    Command::ToggleLyricsProjector => {
                        settings.lyrics_projector = !settings.lyrics_projector;
                        settings.save();
                        with_app(|app| app.apply_settings(settings.clone()));
                        true
                    }
                    Command::ToggleRealBars => {
                        settings.real_bars = !settings.real_bars;
                        settings.save();
                        with_app(|app| app.apply_settings(settings.clone()));
                        true
                    }
                    Command::ToggleHighRefresh => {
                        settings.high_refresh = !settings.high_refresh;
                        settings.save();
                        with_app(|app| app.apply_settings(settings.clone()));
                        true
                    }
                    Command::ToggleTranslucent => {
                        settings.translucent = !settings.translucent;
                        settings.save();
                        with_app(|app| app.apply_settings(settings.clone()));
                        true
                    }
                    Command::BlurRadius(radius) => {
                        settings.blur_radius = radius;
                        settings.translucent = true;
                        settings.save();
                        with_app(|app| app.apply_settings(settings.clone()));
                        true
                    }
                    Command::ToggleExpandOnHover => {
                        settings.expand_on_hover = !settings.expand_on_hover;
                        settings.save();
                        with_app(|app| {
                            app.expanded_by_click = false;
                            app.apply_settings(settings.clone());
                            app.update_expansion();
                        });
                        true
                    }
                    Command::SetHotkey(h) => {
                        settings.hotkey = h;
                        settings.save();
                        with_app(|app| app.apply_settings(settings.clone()));
                        true
                    }
                    Command::ToggleFullscreen => {
                        settings.hide_fullscreen = !settings.hide_fullscreen;
                        settings.save();
                        with_app(|app| app.apply_settings(settings.clone()));
                        true
                    }
                });
            }
            LRESULT(0)
        }
        // Al hacer clic, no quitarle el foco a la ventana que estás usando.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        fullscreen::WM_APPBAR if wparam.0 == fullscreen::ABN_FULLSCREENAPP => {
            let active = fullscreen::is_active(hwnd);
            with_app(|app| app.set_fullscreen(active));
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(Some(hwnd), FULLSCREEN_TIMER);
            }
            fullscreen::unregister(hwnd);
            with_app(|app| app.tray.remove());
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // El Explorador se reinició: la bandeja es nueva y hay que volver a poner el ícono.
        m if tray::is_taskbar_created(m) => {
            with_app(|app| app.tray.add());
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Pruebas de rendimiento: la isla tiene que quedar en 0 % de CPU en reposo y trabajar lo
/// mínimo cuando algo se mueve.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "prueba prolongada de refresco alto; NOTCH_STRESS_SECONDS controla su duración"]
    fn refresco_alto_no_satura_la_cola_ni_bloquea_temporizadores() {
        use std::cell::Cell;
        thread_local! {
            static DRIVER: RefCell<Option<HighRefreshDriver>> = const { RefCell::new(None) };
            static FRAMES: Cell<u64> = const { Cell::new(0) };
            static TIMERS: Cell<u64> = const { Cell::new(0) };
        }
        extern "system" fn stress_proc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            if msg == WM_ANIM_TICK {
                FRAMES.set(FRAMES.get() + 1);
                // Cuadro costoso y DwmFlush que retorna inmediatamente: caso adverso real.
                std::thread::sleep(Duration::from_millis(3));
                DRIVER.with(|driver| driver.borrow().as_ref().unwrap().frame_processed());
                return LRESULT(0);
            }
            if msg == WM_TIMER {
                TIMERS.set(TIMERS.get() + 1);
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        unsafe {
            let instance = GetModuleHandleW(None).unwrap();
            let class = w!("NotchIslaStressRefresh");
            RegisterClassExW(&WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(stress_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class,
                class,
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .unwrap();
            let real_vsync = std::env::var_os("NOTCH_REAL_VSYNC").is_some();
            let hz = if real_vsync {
                monitors::refresh_rate(monitors::find(""))
            } else {
                240
            };
            let driver = if real_vsync {
                HighRefreshDriver::new(hwnd)
            } else {
                HighRefreshDriver::with_sync(hwnd, || Ok(()))
            };
            let running = Arc::clone(&driver.running);
            driver
                .interval_ns
                .store(1_000_000_000 / hz as u64, Ordering::Relaxed);
            driver.set_active(true);
            DRIVER.with(|cell| *cell.borrow_mut() = Some(driver));
            SetTimer(Some(hwnd), 99, 20, None);
            let seconds = std::env::var("NOTCH_STRESS_SECONDS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(10)
                .max(1);
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(seconds) {
                let mut msg = MSG::default();
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    break;
                }
                DispatchMessageW(&msg);
                ui::promote_timers();
            }
            DRIVER.with(|cell| cell.borrow_mut().take());
            let _ = KillTimer(Some(hwnd), 99);
            let _ = DestroyWindow(hwnd);
            let frames = FRAMES.get();
            let timers = TIMERS.get();
            assert!(
                frames > seconds * 10 && frames <= seconds * (hz as u64 * 110 / 100 + 1),
                "cuadros limitados al ritmo pedido: {frames} en {seconds} s"
            );
            assert!(
                timers >= seconds * 15,
                "los temporizadores deben seguir funcionando: {timers} en {seconds} s"
            );
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(
                Arc::strong_count(&running),
                1,
                "el hilo de refresco debe terminar al cerrar"
            );
            eprintln!(
                "{seconds} s: {frames} cuadros, {timers} temporizadores; objetivo {hz} Hz, VSync real={real_vsync}"
            );
        }
    }

    #[test]
    fn en_reposo_no_hay_temporizador() {
        // Nada se mueve: el temporizador se apaga (0 % de CPU).
        assert_eq!(timer_for(&Motion::default(), false, FRAME_MS), None);
        // Cada cosa pide solo el ritmo que necesita.
        let only = |m: Motion| timer_for(&m, false, FRAME_MS);
        let progress = Motion {
            progress: true,
            ..Default::default()
        };
        assert_eq!(only(progress), Some(PROGRESS_MS));
        let bars = Motion {
            bars: true,
            ..Default::default()
        };
        assert_eq!(only(bars), Some(BARS_MS));
        let painting = Motion {
            lyric_painting: true,
            ..Default::default()
        };
        assert_eq!(only(painting), Some(PAINT_MS));
        let sliding = Motion {
            lyric_sliding: true,
            ..Default::default()
        };
        assert_eq!(only(sliding), Some(FRAME_MS));
        assert_eq!(
            timer_for(&Motion::default(), true, FRAME_MS),
            Some(FRAME_MS)
        );
        // Con alta tasa de refresco (por ejemplo 240 Hz = 4 ms, 120 Hz = 8 ms):
        assert_eq!(timer_for(&Motion::default(), true, 4), Some(4));
        assert_eq!(timer_for(&Motion::default(), true, 8), Some(8));
        // Con letra, despierta justo al verso siguiente aunque no haya otra cosa.
        let verse = Motion {
            lyric_ms: Some(1200),
            ..Default::default()
        };
        assert_eq!(only(verse), Some(1200));
        let both = Motion {
            progress: true,
            lyric_ms: Some(100),
            ..Default::default()
        };
        assert_eq!(only(both), Some(100));
        // Con la franja del borde y proyector de letra activo:
        let projecting = Motion {
            projecting: true,
            edge_ms: Some(1000),
            ..Default::default()
        };
        assert_eq!(only(projecting), Some(FRAME_MS));
        let strip_verse = Motion {
            edge_ms: Some(1000),
            lyric_ms: Some(300),
            ..Default::default()
        };
        assert_eq!(only(strip_verse), Some(300));
        let strip_edge_first = Motion {
            edge_ms: Some(400),
            lyric_ms: Some(1500),
            ..Default::default()
        };
        assert_eq!(only(strip_edge_first), Some(400));
    }

    #[test]
    fn zonas_se_unen_y_recortan() {
        let a = Area::new((-5.0, 2.2, 10.5, 8.0), 100, 50);
        assert_eq!(
            a,
            Area {
                x0: 0,
                y0: 2,
                x1: 11,
                y1: 8
            }
        );
        assert!(Area::new((200.0, 0.0, 300.0, 10.0), 100, 50).is_empty());
        let b = Area {
            x0: 20,
            y0: 0,
            x1: 30,
            y1: 4,
        };
        assert_eq!(
            a.union(b),
            Area {
                x0: 0,
                y0: 0,
                x1: 30,
                y1: 8
            }
        );
        assert_eq!(Area::EMPTY.union(b), b);
    }

    #[test]
    fn contraida_trabaja_una_parte_minima_de_la_ventana() {
        let scale = 1.25;
        let (lw, lh) = island::window_size();
        let (w, h) = ((lw * scale).ceil() as u32, (lh * scale).ceil() as u32);
        let zone = |p: f32| {
            let b = island::bounds(w as f32, h as f32, scale, p, 1.0, (0.0, 0.0));
            Area::new(b, w, h)
        };
        let collapsed = zone(0.0).pixels() as f32 / (w * h) as f32;
        // Las barras de la isla contraída (30 veces por segundo) tocan menos del 6 % de la ventana.
        assert!(collapsed < 0.06, "contraida: {:.1} %", collapsed * 100.0);
        // Expandida cubre la isla entera.
        assert!(zone(1.0).pixels() > zone(0.0).pixels() * 5);
    }

    extern "system" fn default_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(h, m, w, l) }
    }

    /// Mide cuánto cuesta mandar un cuadro a Windows (se ignora por defecto: crea una
    /// ventana de verdad un momento).
    #[test]
    #[ignore]
    fn medir_envio_a_windows() {
        unsafe {
            let class = w!("NotchIslaPrueba");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(default_proc),
                hInstance: GetModuleHandleW(None).unwrap().into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!(""),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let (w, h) = (723u32, 217u32);
            let mut surface = Surface::new(w, h).unwrap();
            for px in surface
                .pixmap
                .data_mut()
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .take(5000)
            {
                *px = [20, 20, 20, 255];
            }
            surface.copy_to_dib();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let pos = POINT { x: -2000, y: -2000 };
            let size = SIZE {
                cx: w as i32,
                cy: h as i32,
            };
            let src = POINT::default();
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let small = RECT {
                left: 300,
                top: 0,
                right: 420,
                bottom: 60,
            };
            let runs = 300;
            let cases: [(&str, Option<&RECT>); 2] =
                [("ventana entera", None), ("solo la isla", Some(&small))];
            for (name, dirty) in cases {
                let start = Instant::now();
                for _ in 0..runs {
                    let info = UPDATELAYEREDWINDOWINFO {
                        cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
                        hdcDst: HDC::default(),
                        pptDst: &pos,
                        psize: &size,
                        hdcSrc: surface.dc,
                        pptSrc: &src,
                        crKey: COLORREF(0),
                        pblend: &blend,
                        dwFlags: ULW_ALPHA,
                        prcDirty: dirty.map_or(std::ptr::null(), |r| r as *const RECT),
                    };
                    assert!(UpdateLayeredWindowIndirect(hwnd, &info).as_bool());
                }
                let ms = start.elapsed().as_secs_f64() * 1000.0 / runs as f64;
                println!("{name}: {ms:.3} ms por cuadro");
            }
            let _ = DestroyWindow(hwnd);
        }
    }

    /// Mide un cuadro completo de la isla expandida con letra: fondo, textos y versos
    /// (se ignora por defecto: es una medición, no una prueba).
    #[test]
    #[ignore]
    fn medir_cuadro_con_letra() {
        let scale = 1.25;
        let (lw, lh) = island::window_size();
        let (w, h) = ((lw * scale).ceil() as u32, (lh * scale).ceil() as u32);
        let mut surface = Surface::new(w, h).unwrap();
        // Como en `main`: WIC (la imagen del texto) necesita COM en el hilo.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let mut text = TextRenderer::new(scale).unwrap();
        let verse = |i: i32| (i, format!("Y soy solamente un perdedor numero {i}"));
        let runs = 200;
        let mut totals = [0.0f64; 2];
        for i in 0..runs {
            let start = Instant::now();
            surface.clear(surface.full());
            let areas = island::draw(
                &mut surface.pixmap,
                scale,
                1.0,
                1.0,
                (1.0, 0.0),
                Some(island::Controls {
                    playing: true,
                    hovered: None,
                    seek_hover: None,
                    accent: [200, 120, 60],
                    progress: Some(0.4),
                    volume: None,
                    sessions: (0, 1),
                    cover_hovered: false,
                    pressed: None,
                    update: false,
                    update_hovered: false,
                    levels: None,
                    lyrics_active: true,
                }),
                None,
                None,
                None,
                0.0,
                Some(island::TimeInfo {
                    current: "01:10".into(),
                    remaining: "-01:57".into(),
                }),
                Some(island::LyricsInfo {
                    lines: (-2..=3).map(verse).collect(),
                    shift: 0.3,
                    sung: Some(10.0 + (i % 20) as f32),
                    status: None,
                }),
                false,
            )
            .unwrap();
            surface.copy_to_dib();
            let mid = Instant::now();
            let island = Area::new(
                island::bounds(w as f32, h as f32, scale, 1.0, 1.0, (1.0, 0.0)),
                w,
                h,
            );
            let zone = (island.x0, island.y0, island.x1, island.y1);
            text.draw(
                surface.dib(),
                w,
                h,
                zone,
                &areas,
                "Para No Verte Mas",
                "La Mosca",
                0.0,
            );
            totals[0] += (mid - start).as_secs_f64();
            totals[1] += mid.elapsed().as_secs_f64();
        }
        let ms = |t: f64| t * 1000.0 / runs as f64;
        println!(
            "fondo y copia: {:.3} ms, textos: {:.3} ms por cuadro",
            ms(totals[0]),
            ms(totals[1])
        );
        // Desglose: solo título (sin letra ni tiempos) y nada de texto.
        let only_title = island::LayoutAreas {
            text: island::TextArea {
                x: 100.0,
                w: 200.0,
                cy: 60.0,
                alpha: 1.0,
            },
            time: None,
            lyrics: None,
            seek: None,
        };
        for (name, title) in [("solo titulo", "Para No Verte Mas"), ("sin texto", "")] {
            let start = Instant::now();
            for _ in 0..runs {
                text.draw(
                    surface.dib(),
                    w,
                    h,
                    (0, 0, w, h),
                    &only_title,
                    title,
                    "",
                    0.0,
                );
            }
            println!("{name}: {:.3} ms", ms(start.elapsed().as_secs_f64()));
        }
    }

    #[test]
    fn el_texto_se_dibuja_y_se_reutiliza() {
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let (w, h) = (300u32, 80u32);
        let mut surface = Surface::new(w, h).unwrap();
        // Fondo negro opaco, como la isla.
        for px in surface.pixmap.data_mut().as_chunks_mut::<4>().0 {
            *px = [0, 0, 0, 255];
        }
        let mut text = TextRenderer::new(1.0).unwrap();
        let areas = island::LayoutAreas {
            text: island::TextArea {
                x: 10.0,
                w: 280.0,
                cy: 40.0,
                alpha: 1.0,
            },
            time: None,
            lyrics: None,
            seek: None,
        };
        let frame = |surface: &mut Surface, text: &mut TextRenderer| {
            surface.copy_to_dib();
            let start = Instant::now();
            text.draw(
                surface.dib(),
                w,
                h,
                (0, 0, w, h),
                &areas,
                "Notch Isla",
                "Prueba",
                0.0,
            );
            start.elapsed()
        };
        let first = frame(&mut surface, &mut text);
        // El título se ve: hay píxeles claros sobre el negro.
        let bright = surface
            .dib()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[1] > 128)
            .count();
        assert!(bright > 30, "pixeles de texto: {bright}");
        // El mismo cuadro otra vez: se reutiliza lo dibujado (igual de claro y más barato).
        let mut again = Duration::ZERO;
        for _ in 0..20 {
            again += frame(&mut surface, &mut text);
        }
        let repeat_bright = surface
            .dib()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[1] > 128)
            .count();
        assert_eq!(repeat_bright, bright);
        assert!(
            again / 20 < first,
            "repetido: {:?}, primero: {first:?}",
            again / 20
        );
    }

    #[test]
    fn copia_solo_la_zona_pedida() {
        let mut surface = Surface::new(8, 4).unwrap();
        surface.pixmap.data_mut().fill(9);
        surface.copy_to_dib();
        // Rojo opaco (RGBA) en toda la imagen, y se copia solo el centro.
        for px in surface.pixmap.data_mut().as_chunks_mut::<4>().0 {
            *px = [255, 0, 0, 255];
        }
        let area = Area {
            x0: 2,
            y0: 1,
            x1: 5,
            y1: 3,
        };
        surface.copy_area(area);
        let dib = unsafe { std::slice::from_raw_parts(surface.bits, 8 * 4 * 4) };
        let at = |x: usize, y: usize| dib[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4].to_vec();
        // BGRA dentro de la zona; fuera, lo de antes sin tocar.
        assert_eq!(at(3, 2), [0, 0, 255, 255]);
        assert_eq!(at(0, 0), [9, 9, 9, 9]);
        surface.clear(area);
        let px = |x: usize, y: usize| surface.pixmap.data()[(y * 8 + x) * 4..][..4].to_vec();
        assert_eq!(px(3, 2), [0, 0, 0, 0]);
        assert_eq!(px(0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn limites_de_la_isla_en_pantalla() {
        let rc = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let (lw, _) = island::window_size();
        let scale = 1.0;
        let width = (lw * scale).ceil() as u32;
        let (min_x, max_x) = island_bounds(&rc, width, scale);
        let center_x = rc.left + ((rc.right - rc.left) - width as i32) / 2;

        // La posición centrada queda dentro de los límites:
        assert!(center_x >= min_x && center_x <= max_x);

        // Al margen máximo izquierdo y derecho, la isla expandida no se sale de la pantalla:
        let half_w = width as i32 / 2;
        let min_cx = min_x + half_w;
        let max_cx = max_x + half_w;
        let max_island_w = (island::EXPANDED_LYRICS.0 * scale).round() as i32;
        assert!(min_cx - max_island_w / 2 >= rc.left);
        assert!(max_cx + max_island_w / 2 <= rc.right);
    }
}
