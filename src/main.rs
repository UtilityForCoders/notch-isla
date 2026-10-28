// En release no se abre consola; en debug sí, para ver mensajes.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appicon;
mod audio;
mod autostart;
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
mod system;
mod text;
mod tray;
mod update;
mod visualizer;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::NIN_BALLOONUSERCLICK;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer,
    LoadCursorW, PostQuitMessage, RegisterClassExW, SetTimer, ShowWindow, TranslateMessage,
    UpdateLayeredWindow, IDC_ARROW, MA_NOACTIVATE, MB_ICONASTERISK, MSG, SW_HIDE,
    SW_SHOWNOACTIVATE, ULW_ALPHA, WM_CONTEXTMENU, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_HOTKEY, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONUP, WM_TIMER,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// Aviso de "el mouse salió de la ventana" (definido aquí para no cargar todo el módulo Win32_UI_Controls).
const WM_MOUSELEAVE: u32 = 0x02A3;

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
/// 4 fps para la barra de progreso: avanza pocos píxeles por segundo, no hace falta más.
const PROGRESS_MS: u32 = 250;

/// Cuánto cambia el volumen por cada "clic" de la rueda (como Windows: 2 %).
const VOLUME_STEP: f32 = 0.02;
/// Cuánto tiempo se muestra el nivel de volumen después del último giro de la rueda.
const VOLUME_SHOW: Duration = Duration::from_millis(1200);

/// Qué sigue moviéndose después de dibujar un cuadro.
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
    APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => match guard.as_mut() {
            Some(app) => {
                f(app);
                true
            }
            None => false,
        },
        Err(_) => false,
    })
}

struct App {
    hwnd: HWND,
    surface: Surface,
    /// Posición de la ventana en pantalla (píxeles físicos).
    pos: POINT,
    scale: f32,
    spring: Spring,
    hovered: bool,
    /// Intervalo del temporizador de animación, o `None` si está apagado (reposo).
    timer_ms: Option<u32>,
    last_tick: Instant,
    /// Reloj de las barras de audio.
    started: Instant,
    /// Si DirectWrite fallara, la isla sigue funcionando sin texto.
    text: Option<TextRenderer>,
    media: Option<NowPlaying>,
    /// Botón bajo el mouse, para resaltarlo.
    hovered_button: Option<Button>,
    /// El mouse está sobre la barra de progreso.
    seek_hovered: bool,
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
    /// Transición suave de anchura: 0 = reproductor compacto, 1 = tarjeta amplia con letra.
    lyrics_spring: Spring,
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
    lyrics: HashMap<String, Vec<(f32, String)>>,
    /// Canciones cuya letra se está pidiendo ahora.
    lyrics_pending: HashSet<String>,
    /// Proyector de la letra bajo la isla. Se crea la primera vez que hay un verso que mostrar
    /// (con la letra apagada no ocupa memoria); si no se pudiera crear, la isla sigue igual.
    projector: Option<Projector>,
    /// Escucha el audio de la app que suena para las barras reales.
    visualizer: Visualizer,
}

/// Aviso del sistema con sus textos.
struct ActiveNotice {
    view: Notice,
    title: String,
    subtitle: String,
}

impl App {
    fn new(hwnd: HWND, monitor: HMONITOR, settings: Settings) -> Result<Self> {
        let (surface, pos, scale) = layout_for(monitor)?;
        Ok(Self {
            hwnd,
            surface,
            pos,
            scale,
            spring: Spring::new(0.0, EXPAND_SPRING.0, EXPAND_SPRING.1),
            hovered: false,
            timer_ms: None,
            last_tick: Instant::now(),
            started: Instant::now(),
            text: TextRenderer::new(scale).ok(),
            media: None,
            hovered_button: None,
            seek_hovered: false,
            cover_hovered: false,
            update_hovered: false,
            tray: Tray::new(hwnd),
            scroll_start: Instant::now(),
            fullscreen: false,
            visible: false,
            shown: false,
            presence: Spring::new(0.0, 260.0, 22.0),
            lyrics_spring: Spring::new(if settings.lyrics { 1.0 } else { 0.0 }, 260.0, 22.0),
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
            projector: None,
            visualizer: Visualizer::default(),
        })
    }

    /// Pide la letra de la canción actual si está activada y aún no se pidió.
    fn request_lyrics(&mut self) {
        if !self.settings.lyrics {
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

    /// Expandida mientras el mouse está encima o hay un aviso.
    fn update_expansion(&mut self) {
        let expand = self.visible && (self.hovered || self.lingering || self.notice.is_some());
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
        let previous_hotkey = self.settings.hotkey;
        self.settings = settings;
        if moved {
            self.relayout();
        }
        if self.settings.hotkey != previous_hotkey {
            self.set_hotkey(self.settings.hotkey, previous_hotkey);
        }
        // Recién activada: se pide la letra de lo que suena ahora.
        self.request_lyrics();
        self.lyrics_spring.set_target(if self.settings.lyrics { 1.0 } else { 0.0 });
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
        if let Ok((surface, pos, scale)) = layout_for(monitor) {
            self.surface = surface;
            self.pos = pos;
            self.scale = scale;
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
            self.settings.lyrics = !self.settings.lyrics;
            self.settings.save();
            self.lyrics_spring.set_target(if self.settings.lyrics { 1.0 } else { 0.0 });
            if self.settings.lyrics {
                self.request_lyrics();
            }
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

    fn volume_showing(&self) -> bool {
        self.volume_until.is_some_and(|t| Instant::now() < t)
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

    /// Redibuja fuera de la animación; si algo quedó moviéndose, arranca el temporizador.
    /// Si ya está corriendo, `tick` ajusta el intervalo en el siguiente cuadro.
    fn redraw(&mut self) {
        let motion = self.render();
        if self.timer_ms.is_none() {
            self.set_timer(timer_for(&motion, false));
        }
    }

    /// Enciende, cambia de intervalo o apaga el temporizador de animación.
    fn set_timer(&mut self, ms: Option<u32>) {
        if self.timer_ms == ms {
            return;
        }
        unsafe {
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
        self.timer_ms = ms;
    }

    /// La isla se muestra si hay un aviso, o si hay música y no se ocultó sola; en ambos casos,
    /// salvo que haya algo a pantalla completa (si así está en los ajustes).
    fn update_visibility(&mut self) {
        let want = !self.hidden_by_fullscreen()
            && (self.notice.is_some() || (self.media.is_some() && !self.idle_hidden));
        // Oculta por inactividad, la ventana sigue mostrada para la franja del borde.
        if self.edge_strip() && !self.shown {
            self.render();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            self.shown = true;
        }
        // Ya no corresponde la franja (pantalla completa) y la isla no está: se oculta la ventana.
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
            self.presence.set_target(1.0);
            self.start_idle_countdown();
        } else {
            self.stop_idle_countdown();
            // Sale hacia arriba y a la vez se contrae; la ventana se oculta al terminar (en `tick`).
            self.presence.set_target(0.0);
            self.hovered = false;
            self.lingering = false;
            self.hovered_button = None;
            self.seek_hovered = false;
            self.cover_hovered = false;
            self.update_hovered = false;
            self.aim_expansion(false);
        }
        self.start_animation();
    }

    fn set_fullscreen(&mut self, fullscreen: bool) {
        self.fullscreen = fullscreen;
        self.update_visibility();
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
            self.lyrics_spring.value,
            // Mientras se muestra el volumen, la barra no sirve para saltar.
            media.timeline.is_some() && !self.volume_showing(),
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
        let seek = matches!(hit, Some(Hit::Seek(_)));
        let cover = hit == Some(Hit::Cover);
        let update = hit == Some(Hit::Update);
        if self.hovered_button == button
            && self.seek_hovered == seek
            && self.cover_hovered == cover
            && self.update_hovered == update
        {
            return;
        }
        self.hovered_button = button;
        self.seek_hovered = seek;
        self.cover_hovered = cover;
        self.update_hovered = update;
        self.redraw();
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
        self.lingering = !hovered && self.visible;
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
        self.update_expansion();
    }

    fn start_animation(&mut self) {
        self.set_timer(Some(FRAME_MS));
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;

        let expanding = self.spring.step(dt);
        let entering = self.presence.step(dt);
        let lyrics_moving = self.lyrics_spring.step(dt);
        let moving = expanding || entering || lyrics_moving;
        let motion = self.render();

        // Terminó de salir: ahora sí se oculta la ventana (salvo que deba quedar la franja).
        if !self.visible && !entering && self.shown && !self.edge_strip() {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.shown = false;
        }

        // El temporizador sigue solo mientras algo se mueve, y al ritmo que eso necesita.
        self.set_timer(timer_for(&motion, moving));
    }

    /// Dibuja el cuadro actual y dice qué sigue moviéndose.
    fn render(&mut self) -> Motion {
        // Si la música acaba de terminar, la isla sale mostrando la última canción, no vacía.
        let media = self.media.as_ref().or(self.last_media.as_ref());
        let cover = media.and_then(|m| m.cover.as_ref());
        let volume = self.volume_showing().then_some(self.volume_level);
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
        let playing = self.visible && self.media.as_ref().is_some_and(|m| m.playing);
        let bars = playing && island::mini_alpha(self.spring.value) > 0.0;
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
            seek_hovered: self.seek_hovered,
            accent: cover.map_or([255, 255, 255], |c| c.accent),
            progress: m.timeline.map(|t| t.progress(m.playing)),
            volume,
            sessions: m.sessions,
            cover_hovered: self.cover_hovered,
            update: self.update.is_some(),
            update_hovered: self.update_hovered,
            lyrics_active: self.settings.lyrics,
        });
        // El contenido expandido (y con él la barra) se ve a partir de ~55 % de la expansión.
        // Mientras se muestra el volumen también hace falta redibujar: para quitarlo a tiempo.
        let progress = (playing
            && self.spring.value > 0.55
            && self.media.as_ref().is_some_and(|m| m.timeline.is_some()))
            || volume.is_some();
        // Ancho del texto del aviso, para centrar ícono y texto en la isla.
        let notice_text_w = match (notice, self.text.as_mut()) {
            (Some(n), Some(text)) => text.measure(&n.title, &n.subtitle),
            _ => 0.0,
        };
        let time_info = media.and_then(|m| m.timeline).map(|t| {
            let cur_secs = t.current_position(playing).round() as u32;
            let rem_secs = (t.duration - t.current_position(playing)).max(0.0).round() as u32;
            island::TimeInfo {
                current: format!("{:02}:{:02}", cur_secs / 60, cur_secs % 60),
                remaining: format!("-{:02}:{:02}", rem_secs / 60, rem_secs % 60),
            }
        });
        let lyrics_info = media.map(|m| {
            let key = lyrics::key(&m.title, &m.artist);
            let lines = self.lyrics.get(&key);
            let (prev, current, next, next2, status) = if !self.settings.lyrics && self.lyrics_spring.value <= 0.01 {
                (None, None, None, None, None)
            } else if let Some(lines) = lines {
                if lines.is_empty() {
                    if self.settings.lyrics {
                        (None, None, None, None, Some("Sin letra sincronizada".to_string()))
                    } else {
                        (None, None, None, None, None)
                    }
                } else if let Some(t) = m.timeline {
                    let secs = t.current_position(m.playing);
                    let next_i = lines.partition_point(|(start, _)| *start <= secs);
                    let curr_i = next_i.checked_sub(1);
                    let prev = curr_i.and_then(|i| i.checked_sub(1)).and_then(|i| lines.get(i)).map(|(_, s)| s.clone());
                    let curr = curr_i.and_then(|i| lines.get(i)).map(|(_, s)| s.clone());
                    let next = curr_i.map(|i| i + 1).and_then(|i| lines.get(i)).map(|(_, s)| s.clone());
                    let next2 = curr_i.map(|i| i + 2).and_then(|i| lines.get(i)).map(|(_, s)| s.clone());
                    (prev, curr, next, next2, None)
                } else {
                    (None, None, None, None, None)
                }
            } else if self.lyrics_pending.contains(&key) {
                if self.settings.lyrics {
                    (None, None, None, None, Some("Buscando letra...".to_string()))
                } else {
                    (None, None, None, None, None)
                }
            } else if self.settings.lyrics {
                (None, None, None, None, Some("Sin letra sincronizada".to_string()))
            } else {
                (None, None, None, None, None)
            };
            island::LyricsInfo {
                prev,
                current,
                next,
                next2,
                status,
            }
        });
        let areas = island::draw(
            &mut self.surface.pixmap,
            self.scale,
            self.spring.value,
            self.presence.value,
            self.lyrics_spring.value,
            controls,
            notice.map(|n| (&n.view, notice_text_w)),
            cover.map(|c| c.image.as_ref()),
            media
                .and_then(|m| m.app_icon.as_ref())
                .map(|i| i.0.as_ref()),
            self.started.elapsed().as_secs_f32(),
            time_info,
            lyrics_info,
        );
        let strip = self.edge_strip();
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
        self.surface.copy_to_dib();

        // Proyector de la letra debajo de la isla (solo si la isla está contraída u oculta en el borde):
        let mut lyric_ms = None;
        let should_project = self.spring.value < 0.4 && self.settings.lyrics && playing && (self.visible || strip);
        let lyric = if should_project {
            self.media.as_ref().and_then(|m| {
                let lines = self.lyrics.get(&lyrics::key(&m.title, &m.artist))?;
                let t = m.timeline?;
                let secs = t.current_position(true);
                lyric_ms = lyrics::next_start(lines, secs)
                    .map(|next| (((next - secs) * 1000.0) as u32).clamp(FRAME_MS, 5000));
                lyrics::line_at(lines, secs)
            })
        } else if playing && self.settings.lyrics {
            if let Some(m) = &self.media {
                if let (Some(lines), Some(t)) = (
                    self.lyrics.get(&lyrics::key(&m.title, &m.artist)),
                    m.timeline,
                ) {
                    let secs = t.current_position(true);
                    // Despierta al comenzar el próximo verso, también con la letra dentro de la isla.
                    lyric_ms = lyrics::next_start(lines, secs)
                        .map(|next| (((next - secs) * 1000.0) as u32).clamp(FRAME_MS, 5000));
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
        if let Some(projector) = self.projector.as_mut() {
            let island_bottom = island::bottom(
                self.surface.width as f32,
                self.surface.height as f32,
                self.scale,
                self.spring.value,
                self.presence.value,
                self.lyrics_spring.value,
            );
            // Oculta en el borde, el proyector queda bajo la línea de tiempo.
            let bottom = island_bottom.max(4.0 * self.scale);
            let cx = self.pos.x + self.surface.width as i32 / 2;
            let top = self.pos.y + (bottom - 2.0 * self.scale) as i32;
            projecting = projector.render(lyric, cx, top);
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
                    if let Some(level) = volume {
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
                self.surface.dc,
                self.surface.width,
                self.surface.height,
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
        unsafe {
            // Los píxeles 100 % transparentes dejan pasar los clics a lo que hay debajo.
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
        Motion {
            scroll_ms,
            bars,
            progress,
            edge_ms,
            // El último cuadro de la animación se dibuja ya con el botón en reposo.
            pressing: pressed.is_some(),
            projecting,
            lyric_ms,
        }
    }
}

/// Intervalo del temporizador según lo que se mueve: 60 fps para la isla, un cuadro por píxel
/// de avance de un texto largo (y ninguno durante sus pausas), 30 fps si solo bailan las barras,
/// 4 fps si solo avanza la barra de progreso, un cuadro por píxel de avance si la isla está
/// oculta y solo crece la línea del borde, y apagado si todo está quieto. Con letra, además, a más tardar cuando empieza el verso siguiente.
fn timer_for(motion: &Motion, moving: bool) -> Option<u32> {
    let base = if moving || motion.pressing || motion.projecting {
        Some(FRAME_MS)
    } else if motion.bars {
        Some(BARS_MS)
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

/// Calcula escala, superficie de dibujo y posición (arriba al centro) para un monitor.
fn layout_for(monitor: HMONITOR) -> Result<(Surface, POINT, f32)> {
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
    let pos = POINT {
        x: rc.left + ((rc.right - rc.left) - width as i32) / 2,
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
            })
        }
    }

    /// tiny-skia guarda RGBA premultiplicado; Windows espera BGRA premultiplicado.
    fn copy_to_dib(&mut self) {
        let len = (self.width * self.height * 4) as usize;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, len) };
        for (d, s) in dst
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(self.pixmap.data().as_chunks::<4>().0)
        {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
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

/// Menú de salida de audio de la app que suena y, si se elige una, la asigna y lo avisa.
/// Va fuera de `with_app`: el menú corre su propio bucle de mensajes. Si la función de Windows
/// no está disponible o falla, el menú lo dice y nada más cambia.
fn choose_output(app_id: &str) {
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
    let current = if unavailable.is_none() {
        routing::current(&pids)
    } else {
        None
    };
    let Some(choice) = tray::show_output_menu(&label, &outputs, current.as_deref(), unavailable)
    else {
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

/// Coordenadas del mouse en la ventana (con signo, como GET_X_LPARAM / GET_Y_LPARAM).
fn mouse_pos(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as u16 as i16;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16;
    (x as f32, y as f32)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let (x, y) = mouse_pos(lparam);
            with_app(|app| {
                // Oculta: el mouse está en la franja del borde (o sobre la isla mientras sube).
                if app.idle_hidden {
                    app.reveal();
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
                app.set_hovered(false);
                app.set_hover(None);
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
            with_app(|app| match app.hit_at(x, y) {
                _ if app.notice.is_some() => app.end_notice(),
                Some(Hit::Button(button)) => app.press(button),
                Some(Hit::Seek(fraction)) => app.seek(fraction),
                Some(Hit::Cover) => media::next_session(),
                Some(Hit::Update) => pending = app.update.clone(),
                None => {}
            });
            if pending.is_some() {
                apply_update(pending.as_ref());
            }
            LRESULT(0)
        }
        // Clic derecho sobre la isla con música: elegir la salida de audio de esa app.
        WM_RBUTTONUP => {
            let mut app_id = None;
            with_app(|app| {
                if app.visible && app.notice.is_none() {
                    app_id = app.media.as_ref().map(|m| m.app_id.clone());
                }
            });
            if let Some(app_id) = app_id {
                choose_output(&app_id);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == ANIM_TIMER => {
            with_app(|app| app.tick());
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
                let Some(mut settings) = settings else {
                    return LRESULT(0);
                };
                let label = pending.as_ref().map(update::Update::menu_label);
                let monitors = monitors::list();
                let state = tray::MenuState {
                    update: label.as_deref(),
                    settings: &settings,
                    monitors: &monitors,
                    timer_left,
                };
                use tray::Command;
                match tray::show_menu(&state) {
                    Some(Command::ToggleAutostart) => autostart::set(!autostart::is_enabled()),
                    Some(Command::Update) => apply_update(pending.as_ref()),
                    Some(Command::Exit) => unsafe {
                        let _ = DestroyWindow(hwnd);
                    },
                    Some(Command::Timer(minutes)) => {
                        with_app(|app| app.start_countdown(minutes));
                    }
                    Some(Command::CancelTimer) => {
                        with_app(|app| app.cancel_countdown());
                    }
                    Some(Command::ChangeHotkey) => {
                        with_app(|app| app.begin_hotkey_capture());
                    }
                    Some(Command::CheckUpdates) => {
                        with_app(|app| app.check_updates_now());
                    }
                    Some(command) => {
                        match command {
                            Command::HideDelay(secs) => settings.hide_delay = secs,
                            Command::Monitor(i) => settings.monitor = monitors[i].device.clone(),
                            Command::ToggleNotices => settings.notices = !settings.notices,
                            Command::ToggleLyrics => settings.lyrics = !settings.lyrics,
                            Command::ToggleRealBars => settings.real_bars = !settings.real_bars,
                            Command::SetHotkey(h) => settings.hotkey = h,
                            Command::ToggleFullscreen => {
                                settings.hide_fullscreen = !settings.hide_fullscreen
                            }
                            _ => {}
                        }
                        settings.save();
                        with_app(|app| app.apply_settings(settings));
                    }
                    None => {}
                }
            }
            LRESULT(0)
        }
        // Al hacer clic, no quitarle el foco a la ventana que estás usando.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        fullscreen::WM_APPBAR if wparam.0 == fullscreen::ABN_FULLSCREENAPP => {
            let active = fullscreen::is_active(hwnd, lparam);
            with_app(|app| app.set_fullscreen(active));
            LRESULT(0)
        }
        WM_DESTROY => {
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
