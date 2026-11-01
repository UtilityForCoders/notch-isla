//! Ícono en la bandeja del sistema, su menú (ajustes, temporizador, actualización, salir)
//! y los globos de aviso.

use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{w, Result};
use windows::Win32::Foundation::{E_FAIL, HWND};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_USER, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, LoadIconW, RegisterWindowMessageW, HICON,
    ICONINFO, IDI_APPLICATION, SM_CXSMICON, WM_APP,
};

use crate::hotkey::{self, Hotkey};
use crate::menu::{self, Item};
use crate::monitors::{self, Monitor};
use crate::routing::Output;
use crate::settings::{Settings, HIDE_DELAYS};
use crate::{autostart, island};

/// Mensaje que Windows nos manda cuando hay clics sobre el ícono.
pub const WM_TRAY: u32 = WM_APP + 2;
const TRAY_ID: u32 = 1;

const CMD_AUTOSTART: usize = 1;
const CMD_EXIT: usize = 2;
const CMD_UPDATE: usize = 3;
const CMD_NOTICES: usize = 4;
const CMD_FULLSCREEN: usize = 5;
const CMD_CANCEL_TIMER: usize = 6;
const CMD_LYRICS: usize = 7;
const CMD_HOTKEY_CHANGE: usize = 8;
const CMD_HOTKEY_DEFAULT: usize = 9;
const CMD_HOTKEY_OFF: usize = 10;
const CMD_REAL_BARS: usize = 11;
const CMD_CHECK_UPDATES: usize = 12;
const CMD_LYRICS_PROJECTOR: usize = 13;
const CMD_CENTER: usize = 14;
const CMD_HIGH_REFRESH: usize = 15;
const CMD_TRANSLUCENT: usize = 16;
const CMD_EXPAND_ON_HOVER: usize = 17;
/// Las opciones de los submenús van numeradas a partir de estas bases.
const CMD_HIDE_DELAY: usize = 100;
const CMD_MONITOR: usize = 200;
const CMD_TIMER: usize = 300;
/// Menú de salida de audio de la isla.
const CMD_OUTPUT_DEFAULT: usize = 400;
const CMD_OUTPUT: usize = 401;
const CMD_BLUR_RADIUS: usize = 500;
const BLUR_RADII: [u32; 4] = [4, 10, 16, 32];

/// Duraciones del temporizador en minutos.
pub const TIMER_MINUTES: [u32; 6] = [5, 10, 15, 25, 45, 60];

pub enum Command {
    ToggleAutostart,
    Update,
    Exit,
    /// Segundos para ocultarse sola (0 = nunca).
    HideDelay(u32),
    /// Posición en la lista de pantallas.
    Monitor(usize),
    /// Minutos del temporizador.
    Timer(u32),
    CancelTimer,
    ToggleNotices,
    ToggleFullscreen,
    /// Letra dentro de la isla expandida.
    ToggleLyricsIsland,
    /// Letra en el proyector bajo la isla encogida.
    ToggleLyricsProjector,
    ToggleRealBars,
    CheckUpdates,
    /// Escuchar la próxima combinación de teclas como atajo nuevo.
    ChangeHotkey,
    /// Atajo elegido: el de siempre, o `None` para desactivarlo.
    SetHotkey(Option<Hotkey>),
    /// Centrar la isla en la pantalla.
    Center,
    /// Alternar adaptación de animaciones a los Hz del monitor.
    ToggleHighRefresh,
    /// Alternar desenfoque real del fondo.
    ToggleTranslucent,
    ToggleExpandOnHover,
    BlurRadius(u32),
}

/// Lo que el menú necesita saber para marcar las opciones actuales.
pub struct MenuState<'a> {
    /// Texto de la opción de actualizar, si hay una versión nueva; va arriba de todo.
    pub update: Option<&'a str>,
    pub settings: Settings,
    pub monitors: &'a [Monitor],
    /// Minutos que le quedan al temporizador, si hay uno en marcha.
    pub timer_left: Option<u32>,
}

/// Mensaje "TaskbarCreated": llega si el Explorador se reinicia y hay que volver a poner el ícono.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

pub fn is_taskbar_created(msg: u32) -> bool {
    let id = TASKBAR_CREATED.load(Ordering::Relaxed);
    id != 0 && msg == id
}

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Self {
        unsafe {
            TASKBAR_CREATED.store(
                RegisterWindowMessageW(w!("TaskbarCreated")),
                Ordering::Relaxed,
            );
            let icon = make_icon()
                .or_else(|_| LoadIconW(None, IDI_APPLICATION))
                .unwrap_or_default();
            let tray = Self { hwnd, icon };
            tray.add();
            tray
        }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        for (dst, src) in data.szTip.iter_mut().zip("Notch Isla".encode_utf16()) {
            *dst = src;
        }
        data
    }

    pub fn add(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &self.data());
        }
    }

    /// Globo de Windows junto al ícono (en Windows 10/11 aparece como notificación).
    pub fn notify(&self, title: &str, text: &str) {
        let mut data = self.data();
        data.uFlags |= NIF_INFO;
        // Con el ícono de la app (el de la bandeja).
        data.dwInfoFlags = NIIF_USER;
        for (dst, src) in data.szInfoTitle[..63].iter_mut().zip(title.encode_utf16()) {
            *dst = src;
        }
        for (dst, src) in data.szInfo[..255].iter_mut().zip(text.encode_utf16()) {
            *dst = src;
        }
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    pub fn remove(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyIcon(self.icon);
        }
    }
}

/// Muestra el menú de la bandeja junto al cursor y devuelve la opción elegida.
///
/// Genera las opciones del menú para el estado actual.
pub fn build_items(state: &MenuState) -> Vec<Item> {
    let settings = &state.settings;
    let action = |id: usize, label: &str, checked: bool| Item::Action {
        id,
        label: label.to_string(),
        checked,
    };
    let mut items = Vec::new();
    if let Some(label) = state.update {
        items.push(action(CMD_UPDATE, label, false));
        items.push(Item::Separator);
    }

    let delays = HIDE_DELAYS
        .iter()
        .enumerate()
        .map(|(i, &secs)| {
            let label = match secs {
                0 => "Nunca".to_string(),
                s => format!("{s} segundos"),
            };
            action(CMD_HIDE_DELAY + i, &label, settings.hide_delay == secs)
        })
        .collect();
    items.push(Item::Group {
        label: "Ocultarse sola tras".to_string(),
        items: delays,
    });

    if state.monitors.len() > 1 {
        let current = monitors::find(&settings.monitor);
        let screens = state
            .monitors
            .iter()
            .enumerate()
            .map(|(i, m)| action(CMD_MONITOR + i, &m.label, m.handle == current))
            .collect();
        items.push(Item::Group {
            label: "Pantalla".to_string(),
            items: screens,
        });
    }

    let mut timer = Vec::new();
    if let Some(left) = state.timer_left {
        timer.push(action(
            CMD_CANCEL_TIMER,
            &format!("Cancelar (quedan {left} min)"),
            false,
        ));
    }
    for (i, min) in TIMER_MINUTES.iter().enumerate() {
        timer.push(action(CMD_TIMER + i, &format!("{min} minutos"), false));
    }
    items.push(Item::Group {
        label: "Temporizador".to_string(),
        items: timer,
    });

    items.push(Item::Separator);
    // La letra se pide a internet: se dice a quién, para que cada uno decida activarla.
    items.push(Item::Group {
        label: "Mostrar letra de la canción".to_string(),
        items: vec![
            action(CMD_LYRICS, "Con la isla expandida", settings.lyrics_island),
            action(
                CMD_LYRICS_PROJECTOR,
                "Con la isla encogida",
                settings.lyrics_projector,
            ),
            Item::Label("Se busca en lrclib.net".to_string()),
        ],
    });
    items.push(action(
        CMD_REAL_BARS,
        "Barras con el sonido real",
        settings.real_bars,
    ));
    items.push(action(
        CMD_NOTICES,
        "Avisar al cambiar la salida de audio",
        settings.notices,
    ));
    items.push(action(
        CMD_FULLSCREEN,
        "Ocultar en pantalla completa",
        settings.hide_fullscreen,
    ));
    items.push(action(
        CMD_HIGH_REFRESH,
        "Adaptar animaciones a los Hz de la pantalla",
        settings.high_refresh,
    ));
    let mut blur = vec![action(
        CMD_TRANSLUCENT,
        "Activar desenfoque",
        settings.translucent,
    )];
    for (i, label) in ["Suave", "Medio", "Intenso", "Muy intenso"]
        .iter()
        .enumerate()
    {
        blur.push(action(
            CMD_BLUR_RADIUS + i,
            label,
            settings.translucent && settings.blur_radius == BLUR_RADII[i],
        ));
    }
    items.push(Item::Group {
        label: "Desenfoque del fondo".to_string(),
        items: blur,
    });
    items.push(action(
        CMD_CENTER,
        "Centrar en la pantalla",
        settings.offset_x == 0,
    ));
    items.push(action(
        CMD_EXPAND_ON_HOVER,
        "Expandir al pasar el puntero",
        settings.expand_on_hover,
    ));
    items.push(action(
        CMD_AUTOSTART,
        "Iniciar con Windows",
        autostart::is_enabled(),
    ));
    items.push(Item::Separator);
    let current = settings
        .hotkey
        .map_or("desactivado".to_string(), |h| h.name());
    let default_name = format!("{} (predeterminado)", hotkey::DEFAULT.name());
    items.push(Item::Group {
        label: format!("Atajo para mostrar u ocultar: {current}"),
        items: vec![
            action(CMD_HOTKEY_CHANGE, "Cambiar atajo...", false),
            action(
                CMD_HOTKEY_DEFAULT,
                &default_name,
                settings.hotkey == Some(hotkey::DEFAULT),
            ),
            action(CMD_HOTKEY_OFF, "Desactivado", settings.hotkey.is_none()),
        ],
    });
    items.push(Item::Separator);
    // Con una versión nueva ya encontrada, la opción de arriba la instala; esta igual consulta.
    items.push(action(CMD_CHECK_UPDATES, "Buscar actualizaciones", false));
    items.push(action(CMD_EXIT, "Salir", false));
    items
}

fn id_to_command(id: usize, state: &MenuState) -> Option<Command> {
    let pick = |base: usize, len: usize| (base..base + len).contains(&id).then(|| id - base);
    match id {
        CMD_AUTOSTART => Some(Command::ToggleAutostart),
        CMD_UPDATE => Some(Command::Update),
        CMD_EXIT => Some(Command::Exit),
        CMD_NOTICES => Some(Command::ToggleNotices),
        CMD_FULLSCREEN => Some(Command::ToggleFullscreen),
        CMD_CANCEL_TIMER => Some(Command::CancelTimer),
        CMD_LYRICS => Some(Command::ToggleLyricsIsland),
        CMD_LYRICS_PROJECTOR => Some(Command::ToggleLyricsProjector),
        CMD_REAL_BARS => Some(Command::ToggleRealBars),
        CMD_CHECK_UPDATES => Some(Command::CheckUpdates),
        CMD_CENTER => Some(Command::Center),
        CMD_HIGH_REFRESH => Some(Command::ToggleHighRefresh),
        CMD_TRANSLUCENT => Some(Command::ToggleTranslucent),
        CMD_EXPAND_ON_HOVER => Some(Command::ToggleExpandOnHover),
        CMD_HOTKEY_CHANGE => Some(Command::ChangeHotkey),
        CMD_HOTKEY_DEFAULT => Some(Command::SetHotkey(Some(hotkey::DEFAULT))),
        CMD_HOTKEY_OFF => Some(Command::SetHotkey(None)),
        _ => {
            if let Some(i) = pick(CMD_BLUR_RADIUS, BLUR_RADII.len()) {
                Some(Command::BlurRadius(BLUR_RADII[i]))
            } else if let Some(i) = pick(CMD_HIDE_DELAY, HIDE_DELAYS.len()) {
                Some(Command::HideDelay(HIDE_DELAYS[i]))
            } else if let Some(i) = pick(CMD_MONITOR, state.monitors.len()) {
                Some(Command::Monitor(i))
            } else {
                pick(CMD_TIMER, TIMER_MINUTES.len()).map(|i| Command::Timer(TIMER_MINUTES[i]))
            }
        }
    }
}

/// No recibe `&Tray` a propósito: el menú corre su propio bucle de mensajes y la ventana
/// sigue recibiendo mensajes mientras está abierto (no debe haber un préstamo de la app activo).
/// Permite ejecutar comandos de configuración manteniendo el menú abierto e interactivo.
pub fn show_menu<F>(owner: HWND, mut state: MenuState, mut on_command: F)
where
    F: FnMut(Command, &mut Settings) -> bool,
{
    let items = build_items(&state);
    menu::show_tray(owner, items, move |id| {
        let Some(cmd) = id_to_command(id, &state) else {
            return menu::MenuAction::Close;
        };
        let mut settings = state.settings.clone();
        let keep_open = on_command(cmd, &mut settings);
        if keep_open {
            state.settings = settings;
            let new_items = build_items(&state);
            menu::MenuAction::KeepOpen(new_items)
        } else {
            menu::MenuAction::Close
        }
    });
}

pub enum IslandMenuChoice {
    Output(Option<usize>),
    Center,
}

/// Menú contextual sobre la isla: opciones de salida de audio y centrar en la pantalla.
pub fn show_island_menu(
    app: Option<&str>,
    outputs: &[Output],
    current: Option<&str>,
    unavailable: Option<&str>,
    is_centered: bool,
) -> Option<IslandMenuChoice> {
    let mut items = Vec::new();
    if let Some(app) = app {
        items.push(Item::Label(format!("Salida de audio de {app}")));
        items.push(Item::Separator);
        if let Some(reason) = unavailable {
            items.push(Item::Label(reason.to_string()));
        } else {
            items.push(Item::Action {
                id: CMD_OUTPUT_DEFAULT,
                label: "Predeterminada del sistema".to_string(),
                checked: current.is_none(),
            });
            for (i, o) in outputs.iter().enumerate() {
                items.push(Item::Action {
                    id: CMD_OUTPUT + i,
                    label: o.name.clone(),
                    checked: current == Some(o.id.as_str()),
                });
            }
        }
        items.push(Item::Separator);
    }
    items.push(Item::Action {
        id: CMD_CENTER,
        label: "Centrar en la pantalla".to_string(),
        checked: is_centered,
    });
    match menu::show(items)? {
        CMD_CENTER => Some(IslandMenuChoice::Center),
        CMD_OUTPUT_DEFAULT => Some(IslandMenuChoice::Output(None)),
        id if (CMD_OUTPUT..CMD_OUTPUT + outputs.len()).contains(&id) => {
            Some(IslandMenuChoice::Output(Some(id - CMD_OUTPUT)))
        }
        _ => None,
    }
}

/// Convierte el dibujo de `island::tray_icon` en un HICON del tamaño que usa la bandeja.
fn make_icon() -> Result<HICON> {
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).max(16) as u32;
        let pixmap = island::tray_icon(size).ok_or_else(|| windows::core::Error::from(E_FAIL))?;

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;

        // Los íconos usan BGRA con alfa sin premultiplicar.
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (size * size * 4) as usize);
        for (d, px) in dst.as_chunks_mut::<4>().0.iter_mut().zip(pixmap.pixels()) {
            let c = px.demultiply();
            *d = [c.blue(), c.green(), c.red(), c.alpha()];
        }

        // Máscara monocromática en ceros: con alfa de 32 bits Windows usa el canal alfa.
        let mask_bits = vec![0u8; (size.div_ceil(16) * 2 * size) as usize];
        let mask = CreateBitmap(
            size as i32,
            size as i32,
            1,
            1,
            Some(mask_bits.as_ptr().cast()),
        );

        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info);
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}
