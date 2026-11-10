//! Ícono en la bandeja del sistema, su menú (ajustes, temporizador, actualización, salir)
//! y los globos de aviso.

use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{E_FAIL, HWND};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_USER, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, LoadIconW, LoadImageW,
    RegisterWindowMessageW, HICON, ICONINFO, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR,
    SM_CXSMICON, SM_CYSMICON, WM_APP,
};

use crate::hotkey::{self, Hotkey};
use crate::menu::{self, Item};
use crate::monitors::{self, Monitor};
use crate::routing::Output;
use crate::settings::Settings;
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
const CMD_PIN_EXPANDED: usize = 18;
const CMD_BATTERY_NOTICES: usize = 20;
const CMD_BATTERY_REFRESH: usize = 21;
const CMD_CONNECTION_NOTICES: usize = 22;
const CMD_SHARE: usize = 23;
const CMD_SETTINGS: usize = 24;
/// Las opciones de los submenús van numeradas a partir de estas bases.
const CMD_MONITOR: usize = 200;
const CMD_TIMER: usize = 300;
/// Menú de salida de audio de la isla.
const CMD_OUTPUT_DEFAULT: usize = 400;
const CMD_OUTPUT: usize = 401;
const CMD_BLUR_RADIUS: usize = 500;

/// Duraciones del temporizador en minutos.
pub const TIMER_MINUTES: [u32; 6] = [5, 10, 15, 25, 45, 60];

pub enum Command {
    Share,
    ToggleBatteryNotices,
    ToggleConnectionNotices,
    RefreshBattery,
    ToggleAutostart,
    Update,
    Exit,
    /// Posición en la lista de pantallas.
    Monitor(usize),
    /// Minutos del temporizador.
    Timer(u32),
    CancelTimer,
    ToggleNotices,
    ToggleFullscreen,
    /// Letra dentro de la isla expandida.
    ToggleLyricsIsland,
    /// Letra en el proyector bajo la línea de reposo.
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
    TogglePinnedExpanded,
    BlurRadius(u32),
}

/// Lo que el menú necesita saber para marcar las opciones actuales.
pub struct MenuState<'a> {
    /// Texto de la opción de actualizar, si hay una versión nueva.
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
            let icon = load_app_icon()
                .or_else(|_| make_icon())
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
fn build_settings_items(state: &MenuState) -> Vec<Item> {
    let settings = &state.settings;
    let action = |id: usize, label: &str, checked: bool| Item::Action {
        id,
        label: label.to_string(),
        checked,
    };
    let mut items = vec![
        Item::Label(format!("Notch Isla v{}", env!("CARGO_PKG_VERSION"))),
        Item::Separator,
    ];
    let mut update_label = state.update.map(str::to_string);
    crate::with_app(|app| {
        update_label = app.update.as_ref().map(crate::update::Update::menu_label)
    });
    if let Some(label) = update_label.as_deref() {
        items.push(action(CMD_UPDATE, label, false));
        items.push(Item::Separator);
    }

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
    let mut devices = battery_items(
        &crate::battery::Snapshot::default(),
        settings.battery_notices,
        settings.connection_notices,
    );
    crate::with_app(|app| devices = app.battery_group_items());
    items.push(Item::Group {
        label: "Dispositivos y batería".into(),
        items: devices,
    });
    // La letra se pide a internet: se dice a quién, para que cada uno decida activarla.
    items.push(Item::Group {
        label: "Mostrar letra de la canción".to_string(),
        items: vec![
            action(CMD_LYRICS, "Con la isla expandida", settings.lyrics_island),
            action(
                CMD_LYRICS_PROJECTOR,
                "Con la línea de reposo",
                settings.lyrics_projector,
            ),
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
    blur.push(Item::Slider {
        id: CMD_BLUR_RADIUS,
        label: format!("Intensidad: {} %", settings.blur_radius * 100 / 40),
        value: settings.blur_radius,
        min: 0,
        max: 40,
    });
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
        CMD_PIN_EXPANDED,
        "Mantener la isla expandida",
        settings.pinned_expanded,
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
    items.push(action(CMD_SHARE, "Compartir cerca…", false));
    items.push(action(CMD_CHECK_UPDATES, "Buscar actualizaciones", false));
    items.push(action(CMD_EXIT, "Salir", false));
    items
}

fn id_to_command(id: usize, state: &MenuState) -> Option<Command> {
    let pick = |base: usize, len: usize| (base..base + len).contains(&id).then(|| id - base);
    match id {
        CMD_BATTERY_NOTICES => Some(Command::ToggleBatteryNotices),
        CMD_CONNECTION_NOTICES => Some(Command::ToggleConnectionNotices),
        CMD_BATTERY_REFRESH => Some(Command::RefreshBattery),
        CMD_AUTOSTART => Some(Command::ToggleAutostart),
        CMD_UPDATE => Some(Command::Update),
        CMD_EXIT => Some(Command::Exit),
        CMD_NOTICES => Some(Command::ToggleNotices),
        CMD_FULLSCREEN => Some(Command::ToggleFullscreen),
        CMD_CANCEL_TIMER => Some(Command::CancelTimer),
        CMD_LYRICS => Some(Command::ToggleLyricsIsland),
        CMD_LYRICS_PROJECTOR => Some(Command::ToggleLyricsProjector),
        CMD_REAL_BARS => Some(Command::ToggleRealBars),
        CMD_SHARE => Some(Command::Share),
        CMD_CHECK_UPDATES => Some(Command::CheckUpdates),
        CMD_CENTER => Some(Command::Center),
        CMD_HIGH_REFRESH => Some(Command::ToggleHighRefresh),
        CMD_TRANSLUCENT => Some(Command::ToggleTranslucent),
        CMD_EXPAND_ON_HOVER => Some(Command::ToggleExpandOnHover),
        CMD_PIN_EXPANDED => Some(Command::TogglePinnedExpanded),
        CMD_HOTKEY_CHANGE => Some(Command::ChangeHotkey),
        CMD_HOTKEY_DEFAULT => Some(Command::SetHotkey(Some(hotkey::DEFAULT))),
        CMD_HOTKEY_OFF => Some(Command::SetHotkey(None)),
        _ => {
            if let Some(i) = pick(CMD_BLUR_RADIUS, 41) {
                Some(Command::BlurRadius(i as u32))
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
pub fn build_items(_state: &MenuState) -> Vec<Item> {
    vec![
        Item::Action {
            id: CMD_SETTINGS,
            label: "Configuración…".into(),
            checked: false,
        },
        Item::Separator,
        Item::Action {
            id: CMD_EXIT,
            label: "Salir".into(),
            checked: false,
        },
    ]
}

fn categories(state: &MenuState) -> Vec<crate::settings_ui::Category> {
    use crate::settings_ui::Category;
    let mut categories: Vec<Category> = [
        ("General", "Inicio, comportamiento y atajos de teclado"),
        ("Apariencia", "Pantalla, posición y efectos visuales"),
        (
            "Música y letra",
            "Reproducción y letra sincronizada de LRCLIB",
        ),
        ("Dispositivos", "Batería, conexiones y avisos"),
        ("Temporizador", "Elige cuándo quieres recibir un aviso"),
        ("Compartir", "Archivos y portapapeles entre tus equipos"),
        ("Actualizaciones", "Versión instalada y nuevas versiones"),
    ]
    .into_iter()
    .map(|(title, description)| Category {
        title: title.into(),
        description: description.into(),
        items: Vec::new(),
    })
    .collect();
    for item in build_settings_items(state) {
        let index = match &item {
            Item::Separator => continue,
            Item::Label(_) => 6,
            Item::Group { label, .. } => match label.as_str() {
                "Pantalla" | "Desenfoque del fondo" => 1,
                "Mostrar letra de la canción" => 2,
                "Dispositivos y batería" => 3,
                "Temporizador" => 4,
                _ => 0,
            },
            Item::Action { id, .. } => match *id {
                CMD_EXIT => continue,
                CMD_CENTER | CMD_HIGH_REFRESH => 1,
                CMD_REAL_BARS => 2,
                CMD_NOTICES => 3,
                CMD_SHARE => 5,
                CMD_UPDATE | CMD_CHECK_UPDATES => 6,
                _ => 0,
            },
            _ => 0,
        };
        categories[index].items.push(item);
    }
    categories
}

pub(crate) fn is_toggle(id: usize) -> bool {
    matches!(
        id,
        CMD_AUTOSTART
            | CMD_NOTICES
            | CMD_FULLSCREEN
            | CMD_LYRICS
            | CMD_LYRICS_PROJECTOR
            | CMD_REAL_BARS
            | CMD_HIGH_REFRESH
            | CMD_TRANSLUCENT
            | CMD_EXPAND_ON_HOVER
            | CMD_PIN_EXPANDED
            | CMD_BATTERY_NOTICES
            | CMD_CONNECTION_NOTICES
    )
}

pub fn show_menu<F>(mut state: MenuState, mut on_command: F)
where
    F: FnMut(Command, &mut Settings) -> bool,
{
    let mut chosen = None;
    menu::show_tray(build_items(&state), |id| {
        chosen = Some(id);
        menu::MenuAction::Close
    });
    if chosen == Some(CMD_EXIT) {
        on_command(Command::Exit, &mut state.settings);
        return;
    }
    if chosen != Some(CMD_SETTINGS) {
        return;
    }
    // La configuración es una ventana independiente: la isla sigue ocultándose y avisando.
    crate::with_app(|app| {
        app.tray_menu_open = false;
        app.show_next_notice();
    });
    let mut appearance_changed = false;
    let result = crate::settings_ui::show(categories(&state), |id| {
        let mut close = false;
        if let Some(id) = id {
            if let Some(cmd) = id_to_command(id, &state) {
                appearance_changed |= matches!(cmd, Command::BlurRadius(_));
                on_command(cmd, &mut state.settings);
                close = matches!(id, CMD_SHARE | CMD_HOTKEY_CHANGE | CMD_UPDATE);
            }
        }
        crate::with_app(|app| {
            state.settings = app.settings.clone();
            state.timer_left = app.countdown_left();
        });
        (categories(&state), close)
    });
    if let Err(error) = result {
        crate::with_app(|app| {
            app.tray
                .notify("Configuración", &format!("No se pudo abrir: {error}"))
        });
    }
    if appearance_changed {
        state.settings.save_appearance();
    }
}

pub enum IslandMenuChoice {
    Share,
    Output(Option<usize>),
    Center,
    TogglePinnedExpanded,
}

pub fn battery_items(
    snapshot: &crate::battery::Snapshot,
    notices: bool,
    connections: bool,
) -> Vec<Item> {
    let mut items = vec![
        Item::Label("Dispositivos y batería".into()),
        Item::Separator,
        Item::Action {
            id: CMD_CONNECTION_NOTICES,
            label: "Avisar al conectar un dispositivo".into(),
            checked: connections,
        },
        Item::Action {
            id: CMD_BATTERY_NOTICES,
            label: format!(
                "Avisar con batería baja (≤ {} %)",
                crate::battery::LOW_PERCENT
            ),
            checked: notices,
        },
        Item::Action {
            id: CMD_BATTERY_REFRESH,
            label: if snapshot.scanning {
                "Consultando dispositivos…".into()
            } else {
                "Actualizar ahora".into()
            },
            checked: false,
        },
        Item::Separator,
    ];
    if snapshot.updated.is_some()
        && !snapshot
            .devices
            .iter()
            .any(|d| !snapshot.show_inactive_receiver(d))
    {
        items.push(Item::Label("Sin periféricos activos detectados".into()));
    }
    for device in snapshot
        .devices
        .iter()
        .filter(|d| !snapshot.show_inactive_receiver(d))
    {
        let mut status = device
            .level
            .map_or_else(|| "Sin lectura".into(), |level| level.label());
        if snapshot.show_previous_reading(device) {
            status.push_str(" (anterior)");
        } else if device.charging {
            status.push_str(" · Cargando");
        }
        items.push(Item::DeviceStatus {
            name: device.name.clone(),
            status,
            icon: island::device_icon(device.kind),
            connection: match device.connection {
                "Bluetooth" => island::NoticeIcon::Bluetooth,
                "USB / 2.4 GHz" | "Inalámbrico" => island::NoticeIcon::Wireless,
                "USB / receptor" | "USB" => island::NoticeIcon::Usb,
                _ => island::NoticeIcon::Device,
            },
        });
    }
    let inactive: Vec<_> = snapshot
        .devices
        .iter()
        .filter(|d| snapshot.show_inactive_receiver(d))
        .collect();
    if !inactive.is_empty() {
        let mut details = vec![Item::Label("USB conectado; enlace no confirmado".into())];
        for device in &inactive {
            details.push(Item::Label(device.name.clone()));
            if let Some(level) = device.level {
                details.push(Item::Label(format!(
                    "Última lectura por receptor: {}",
                    level.label()
                )));
            }
            if !device.detail.is_empty() {
                details.push(Item::Label(device.detail.clone()));
            }
        }
        details.push(Item::Label("Estas lecturas no generan avisos".into()));
        items.push(Item::Group {
            label: format!("Receptores sin enlace confirmado ({})", inactive.len()),
            items: details,
        });
    }
    if !snapshot.errors.is_empty() {
        items.push(Item::Group {
            label: "No se pudo consultar alguna fuente".into(),
            items: snapshot.errors.iter().cloned().map(Item::Label).collect(),
        });
    }
    items
}

/// El encabezado del submenú reemplaza el título del antiguo panel.
pub fn battery_group_items(
    snapshot: &crate::battery::Snapshot,
    notices: bool,
    connections: bool,
) -> Vec<Item> {
    battery_items(snapshot, notices, connections)
        .into_iter()
        .skip(2)
        .collect()
}

/// Menú contextual sobre la isla: opciones de salida de audio y centrar en la pantalla.
pub fn show_island_menu(
    app: Option<&str>,
    outputs: &[Output],
    current: Option<&str>,
    unavailable: Option<&str>,
    is_centered: bool,
    pinned_expanded: bool,
) -> Option<IslandMenuChoice> {
    let mut items = vec![
        Item::Action {
            id: CMD_SHARE,
            label: "Compartir cerca…".to_string(),
            checked: false,
        },
        Item::Separator,
    ];
    items.push(Item::Action {
        id: CMD_PIN_EXPANDED,
        label: "Fijar versión extendida".to_string(),
        checked: pinned_expanded,
    });
    items.push(Item::Separator);
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
        CMD_SHARE => Some(IslandMenuChoice::Share),
        CMD_PIN_EXPANDED => Some(IslandMenuChoice::TogglePinnedExpanded),
        CMD_CENTER => Some(IslandMenuChoice::Center),
        CMD_OUTPUT_DEFAULT => Some(IslandMenuChoice::Output(None)),
        id if (CMD_OUTPUT..CMD_OUTPUT + outputs.len()).contains(&id) => {
            Some(IslandMenuChoice::Output(Some(id - CMD_OUTPUT)))
        }
        _ => None,
    }
}

/// Carga el recurso 1 de app.rc (assets/icon.ico) al tamaño de la bandeja.
#[allow(clippy::manual_dangling_ptr)]
fn load_app_icon() -> Result<HICON> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        // Sin LR_SHARED: Tray es dueño del HICON y lo libera con DestroyIcon.
        let icon = LoadImageW(
            Some(instance.into()),
            PCWSTR(1usize as *const u16),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON).max(16),
            GetSystemMetrics(SM_CYSMICON).max(16),
            LR_DEFAULTCOLOR,
        )?;
        Ok(HICON(icon.0))
    }
}

/// Dibujo de respaldo si no se puede cargar el ícono incrustado.
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

#[cfg(test)]
mod settings_tests {
    use super::*;

    fn ids(items: &[Item]) -> Vec<usize> {
        let mut out = Vec::new();
        for item in items {
            match item {
                Item::Action { id, .. } | Item::Slider { id, .. } => out.push(*id),
                Item::Group { items, .. } => out.extend(ids(items)),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn configuracion_conserva_cada_opcion_en_una_categoria() {
        let monitors = monitors::list();
        let state = MenuState {
            update: Some("Instalar nueva versión"),
            settings: Settings::load(),
            monitors: &monitors,
            timer_left: Some(12),
        };
        assert_eq!(ids(&build_items(&state)), vec![CMD_SETTINGS, CMD_EXIT]);
        let mut expected = ids(&build_settings_items(&state));
        expected.retain(|id| *id != CMD_EXIT);
        let groups = categories(&state);
        assert_eq!(groups.len(), 7);
        let mut actual: Vec<_> = groups
            .iter()
            .flat_map(|category| ids(&category.items))
            .collect();
        expected.sort_unstable();
        actual.sort_unstable();
        assert_eq!(actual, expected);
        actual.dedup();
        assert_eq!(actual.len(), expected.len());
        for id in actual {
            assert!(id_to_command(id, &state).is_some());
        }
    }

    #[test]
    #[ignore = "ventana de revisión visual; los cambios de esta prueba solo viven en memoria"]
    fn revisar_configuracion() {
        unsafe {
            windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            )
            .ok()
            .unwrap();
        }
        let monitors = monitors::list();
        let mut state = MenuState {
            update: None,
            settings: Settings::load(),
            monitors: &monitors,
            timer_left: None,
        };
        crate::settings_ui::show(categories(&state), |id| {
            if let Some(id) = id {
                match id_to_command(id, &state) {
                    Some(Command::ToggleFullscreen) => state.settings.hide_fullscreen ^= true,
                    Some(Command::ToggleExpandOnHover) => state.settings.expand_on_hover ^= true,
                    Some(Command::TogglePinnedExpanded) => state.settings.pinned_expanded ^= true,
                    Some(Command::ToggleTranslucent) => state.settings.translucent ^= true,
                    Some(Command::ToggleHighRefresh) => state.settings.high_refresh ^= true,
                    Some(Command::ToggleLyricsIsland) => state.settings.lyrics_island ^= true,
                    Some(Command::ToggleLyricsProjector) => state.settings.lyrics_projector ^= true,
                    Some(Command::BlurRadius(value)) => state.settings.blur_radius = value,
                    Some(Command::Timer(minutes)) => state.timer_left = Some(minutes),
                    Some(Command::CancelTimer) => state.timer_left = None,
                    _ => {}
                }
            }
            (categories(&state), false)
        })
        .unwrap();
    }
}
