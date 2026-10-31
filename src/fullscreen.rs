//! Detección de apps a pantalla completa (juegos, videos, presentaciones).
//!
//! Combina el aviso por appbar de Explorer (`ABN_FULLSCREENAPP`) con la API nativa de Windows
//! `SHQueryUserNotificationState` (detecta juegos Direct3D) y la comprobación de dimensiones
//! de la ventana en primer plano (detecta juegos en modo ventana sin bordes / borderless fullscreen).
//! Solo cuenta lo que pasa en el monitor de la isla.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, SHQueryUserNotificationState, ABM_NEW, ABM_REMOVE, APPBARDATA,
    QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowLongW, GetWindowRect, IsIconic, IsWindowVisible,
    GWL_STYLE, WM_APP, WS_CAPTION,
};

/// Mensaje con el que el Explorador nos avisa; `wparam` = tipo de aviso.
pub const WM_APPBAR: u32 = WM_APP + 3;
/// `wparam` de "una app entró o salió de pantalla completa" (`lparam` != 0 = entró).
pub const ABN_FULLSCREENAPP: usize = 2;

fn data(hwnd: HWND) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: WM_APPBAR,
        ..Default::default()
    }
}

pub fn register(hwnd: HWND) {
    let mut d = data(hwnd);
    unsafe {
        SHAppBarMessage(ABM_NEW, &mut d);
    }
}

pub fn unregister(hwnd: HWND) {
    let mut d = data(hwnd);
    unsafe {
        SHAppBarMessage(ABM_REMOVE, &mut d);
    }
}

/// ¿Hay algo a pantalla completa en el monitor de la isla? Un juego Direct3D, una presentación,
/// o una ventana sin barra de título que cubre todo el monitor (juegos en modo "sin bordes",
/// videos del navegador a pantalla completa).
pub fn is_fullscreen(hwnd: HWND) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || fg == hwnd {
            return false;
        }
        // El escritorio y la barra de tareas no cuentan (hacer clic en el escritorio también
        // dispara el aviso del Explorador).
        let mut class = [0u16; 64];
        let len = GetClassNameW(fg, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if class == "WorkerW" || class == "Progman" || class == "Shell_TrayWnd" {
            return false;
        }
        if !IsWindowVisible(fg).as_bool() || IsIconic(fg).as_bool() {
            return false;
        }
        // Solo cuenta si es en el mismo monitor que la isla.
        let monitor = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        if monitor != MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) {
            return false;
        }

        // Juegos Direct3D en modo exclusivo y presentaciones. Es un estado de todo el sistema:
        // por eso se mira después de saber que la ventana activa está en el monitor de la isla.
        if let Ok(state) = SHQueryUserNotificationState() {
            if state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE {
                return true;
            }
        }

        let mut window = RECT::default();
        if GetWindowRect(fg, &mut window).is_err() {
            return false;
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return false;
        }
        let style = GetWindowLongW(fg, GWL_STYLE) as u32;
        covers_monitor(&window, &info.rcMonitor, style)
    }
}

/// Una ventana está a pantalla completa si cubre todo el monitor y no tiene barra de título.
/// Las maximizadas comunes sí la tienen: con la barra de tareas oculta automáticamente también
/// cubren el monitor (sus bordes invisibles sobresalen unos píxeles), pero no son pantalla completa.
fn covers_monitor(window: &RECT, monitor: &RECT, style: u32) -> bool {
    style & WS_CAPTION.0 != WS_CAPTION.0
        && window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

/// Interpreta el aviso del Explorador (`ABN_FULLSCREENAPP`). No se le cree sin más: también
/// avisa al hacer clic en el escritorio y por apps de otro monitor. Se usa la misma regla que
/// el sondeo, así los dos nunca se contradicen y la isla no parpadea.
pub fn is_active(hwnd: HWND) -> bool {
    is_fullscreen(hwnd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE};

    const MONITOR: RECT = RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn maximizada_con_barra_de_tareas_oculta_no_es_pantalla_completa() {
        // Chrome o Roblox maximizados con la barra de tareas en ocultar automáticamente.
        let style = (WS_OVERLAPPEDWINDOW | WS_VISIBLE).0;
        assert!(!covers_monitor(&rect(-8, -8, 1928, 1088), &MONITOR, style));
    }

    #[test]
    fn ventana_sin_bordes_que_cubre_el_monitor_es_pantalla_completa() {
        let style = (WS_POPUP | WS_VISIBLE).0;
        assert!(covers_monitor(&MONITOR, &MONITOR, style));
    }

    #[test]
    fn ventana_sin_bordes_mas_chica_no_es_pantalla_completa() {
        let style = (WS_POPUP | WS_VISIBLE).0;
        assert!(!covers_monitor(&rect(0, 0, 1920, 1040), &MONITOR, style));
    }

    #[test]
    fn monitor_secundario() {
        let second = rect(1920, 0, 3840, 1080);
        let style = (WS_POPUP | WS_VISIBLE).0;
        assert!(covers_monitor(&second, &second, style));
        assert!(!covers_monitor(&MONITOR, &second, style));
    }
}
