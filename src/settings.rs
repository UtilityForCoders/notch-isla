//! Ajustes del menú de la bandeja, guardados en el registro del usuario
//! (HKCU\Software\NotchIsla; no requiere permisos de administrador).

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, REG_SZ,
    RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};

use crate::hotkey::{self, Hotkey};

const KEY: PCWSTR = w!("Software\\NotchIsla");

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct IslandColor(pub [u8; 3]);

impl Default for IslandColor {
    fn default() -> Self {
        Self([66, 132, 235])
    }
}

impl IslandColor {
    fn load() -> Self {
        if let Some(rgb) = read_dword(w!("IslandColorRgb")).filter(|rgb| *rgb <= 0xFFFFFF) {
            let [_, r, g, b] = rgb.to_be_bytes();
            return Self([r, g, b]);
        }
        // Las versiones anteriores guardaban el número del color predefinido.
        match read_dword(w!("IslandColor")) {
            Some(1) => Self([128, 128, 128]),
            Some(2) => Self([158, 87, 213]),
            Some(3) => Self([217, 63, 78]),
            Some(4) => Self([60, 169, 105]),
            Some(5) => Self([214, 147, 51]),
            _ => Self::default(),
        }
    }

    pub fn rgb(self) -> u32 {
        let [r, g, b] = self.0;
        u32::from_be_bytes([0, r, g, b])
    }

    /// Mezcla el color elegido con un gris para mantener legibles fondos e iconos.
    pub fn blend(self, gray: u8, strength: u16) -> [u8; 3] {
        self.0.map(|channel| {
            ((u16::from(channel) * strength + u16::from(gray) * (100 - strength)) / 100) as u8
        })
    }
}

#[derive(Clone)]
pub struct Settings {
    pub island_color: IslandColor,
    /// Pantalla elegida (nombre de dispositivo, como "\\.\DISPLAY2"); vacío = la principal.
    pub monitor: String,
    /// Se esconde mientras hay una app a pantalla completa.
    pub hide_fullscreen: bool,
    /// Avisa al cambiar la salida de audio (audífonos, parlantes).
    pub notices: bool,
    pub battery_notices: bool,
    pub connection_notices: bool,
    /// Letra sincronizada dentro de la isla expandida y en el proyector bajo la línea de reposo.
    /// Se activan por separado; apagadas si no se activan (la letra se pide a lrclib.net).
    pub lyrics_island: bool,
    pub lyrics_projector: bool,
    /// Experimental: la letra sin tiempos por palabra avanza con la voz que se oye (ver
    /// `visualizer::VoiceDetector`). Apagada por defecto.
    pub lyrics_voice: bool,
    /// Las barras del reproductor siguen el sonido real de la app (si no, una animación).
    pub real_bars: bool,
    /// Atajo para mostrar u ocultar la isla; `None` = desactivado.
    pub hotkey: Option<Hotkey>,
    /// Desplazamiento horizontal respecto al centro de la pantalla (píxeles lógicos; 0 = centrada).
    pub offset_x: i32,
    /// Adapta la frecuencia de cuadros de las animaciones a los Hz del monitor (120 Hz, 240 Hz, etc.).
    pub high_refresh: bool,
    /// Fondo desenfocado por el compositor con tinte oscuro (si no, negro opaco).
    pub translucent: bool,
    /// Expandir al pasar el puntero; por defecto se requiere un clic.
    pub expand_on_hover: bool,
    /// Mantiene los controles abiertos aunque el puntero salga de la isla.
    pub pinned_expanded: bool,
    /// Desviación del desenfoque gaussiano en píxeles lógicos (0 a 40).
    pub blur_radius: u32,
}

impl Settings {
    pub fn load() -> Self {
        // Sin valor guardado, el atajo de siempre; con tecla 0, desactivado.
        let hotkey = match read_dword(w!("HotkeyKey")) {
            None => Some(hotkey::DEFAULT),
            Some(0) => None,
            Some(vk) => Some(Hotkey {
                mods: read_dword(w!("HotkeyMods")).unwrap_or(0),
                vk,
            }),
        };
        // Antes había una sola opción de letra: quien la tenía activada conserva las dos.
        let lyrics = read_dword(w!("Lyrics")).is_some_and(|v| v != 0);
        Self {
            island_color: IslandColor::load(),
            monitor: read_string(w!("Monitor")).unwrap_or_default(),
            hide_fullscreen: read_dword(w!("HideFullscreen")).is_none_or(|v| v != 0),
            notices: read_dword(w!("Notices")).is_none_or(|v| v != 0),
            battery_notices: read_dword(w!("BatteryNotices")).is_none_or(|v| v != 0),
            connection_notices: read_dword(w!("ConnectionNotices")).is_none_or(|v| v != 0),
            lyrics_island: read_dword(w!("LyricsIsland")).map_or(lyrics, |v| v != 0),
            lyrics_projector: read_dword(w!("LyricsProjector")).map_or(lyrics, |v| v != 0),
            lyrics_voice: read_dword(w!("LyricsVoice")).is_some_and(|v| v != 0),
            real_bars: read_dword(w!("RealBars")).is_none_or(|v| v != 0),
            hotkey,
            offset_x: read_dword(w!("OffsetX")).map_or(0, |v| v as i32),
            high_refresh: read_dword(w!("HighRefresh")).is_some_and(|v| v != 0),
            translucent: read_dword(w!("Translucent")).is_none_or(|v| v != 0),
            expand_on_hover: read_dword(w!("ExpandOnHover")).is_some_and(|v| v != 0),
            pinned_expanded: read_dword(w!("PinnedExpanded")).is_some_and(|v| v != 0),
            blur_radius: read_dword(w!("BlurRadius")).unwrap_or(16).min(40),
        }
    }

    pub fn save(&self) {
        write_dword(w!("IslandColorRgb"), self.island_color.rgb());
        write_string(w!("Monitor"), &self.monitor);
        write_dword(w!("HideFullscreen"), self.hide_fullscreen as u32);
        write_dword(w!("Notices"), self.notices as u32);
        write_dword(w!("BatteryNotices"), self.battery_notices as u32);
        write_dword(w!("ConnectionNotices"), self.connection_notices as u32);
        write_dword(w!("LyricsIsland"), self.lyrics_island as u32);
        write_dword(w!("LyricsProjector"), self.lyrics_projector as u32);
        write_dword(w!("LyricsVoice"), self.lyrics_voice as u32);
        write_dword(w!("RealBars"), self.real_bars as u32);
        write_dword(w!("HotkeyMods"), self.hotkey.map_or(0, |h| h.mods));
        write_dword(w!("HotkeyKey"), self.hotkey.map_or(0, |h| h.vk));
        write_dword(w!("OffsetX"), self.offset_x as u32);
        write_dword(w!("HighRefresh"), self.high_refresh as u32);
        write_dword(w!("Translucent"), self.translucent as u32);
        write_dword(w!("ExpandOnHover"), self.expand_on_hover as u32);
        write_dword(w!("PinnedExpanded"), self.pinned_expanded as u32);
        write_dword(w!("BlurRadius"), self.blur_radius);
    }

    /// Guarda la vista previa del deslizador al cerrar el menú, sin reescribir todo.
    pub fn save_appearance(&self) {
        write_dword(w!("Translucent"), self.translucent as u32);
        write_dword(w!("BlurRadius"), self.blur_radius);
    }

    /// Alguna de las dos letras está activada: hay que pedirla.
    pub fn lyrics(&self) -> bool {
        self.lyrics_island || self.lyrics_projector
    }
}

/// Versión que se está instalando, anotada antes de cerrar la isla para actualizar
/// (`None` la borra). Al volver a abrirse, la isla la compara con la suya.
pub fn set_pending_update(version: Option<&str>) {
    match version {
        Some(v) => write_string(w!("UpdatePending"), v),
        None => unsafe {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, KEY, w!("UpdatePending"));
        },
    }
}

/// Guarda `current` como la última versión que se abrió y devuelve la anterior (si la había).
pub fn swap_last_version(current: &str) -> Option<String> {
    let last = read_string(w!("LastVersion")).filter(|v| !v.is_empty());
    if last.as_deref() != Some(current) {
        write_string(w!("LastVersion"), current);
    }
    last
}

/// Lee y borra la versión anotada por `set_pending_update`.
pub fn take_pending_update() -> Option<String> {
    let version = read_string(w!("UpdatePending")).filter(|v| !v.is_empty());
    if version.is_some() {
        set_pending_update(None);
    }
    version
}

fn read_dword(name: PCWSTR) -> Option<u32> {
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            KEY,
            name,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then_some(value)
}

fn read_string(name: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 260];
    let mut size = std::mem::size_of_val(&buf) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            KEY,
            name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn write_dword(name: PCWSTR, value: u32) {
    unsafe {
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            KEY,
            name,
            REG_DWORD.0,
            Some((&value as *const u32).cast()),
            std::mem::size_of::<u32>() as u32,
        );
    }
}

fn write_string(name: PCWSTR, value: &str) {
    // Termina en 0 como pide el registro.
    let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            KEY,
            name,
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        );
    }
}
