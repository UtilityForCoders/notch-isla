//! Detección de apps a pantalla completa (juegos, videos, presentaciones).
//!
//! Combina el aviso por appbar de Explorer (`ABN_FULLSCREENAPP`) con la API nativa de Windows
//! `SHQueryUserNotificationState` (detecta juegos Direct3D) y la comprobación de dimensiones
//! de la ventana en primer plano (detecta juegos en modo ventana sin bordes / borderless fullscreen).

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, SHQueryUserNotificationState, ABM_NEW, ABM_REMOVE, APPBARDATA,
    QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowRect, IsIconic, IsWindowVisible, WM_APP,
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

/// Comprueba si la ventana activa en primer plano o el sistema está en pantalla completa
/// (juegos DirectX/Vulkan, borderless fullscreen, videos en pantalla completa, presentaciones).
pub fn is_fullscreen(hwnd: HWND) -> bool {
    unsafe {
        // 1. Detección nativa de Windows para juegos Direct3D y presentaciones:
        if let Ok(state) = SHQueryUserNotificationState() {
            if state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE {
                return true;
            }
        }

        // 2. Detección por ventana en primer plano que cubre todo el monitor:
        let fg = GetForegroundWindow();
        if fg.is_invalid() || fg == hwnd {
            return false;
        }

        // Ignorar el escritorio de Windows y la barra de tareas
        let mut class = [0u16; 64];
        let len = GetClassNameW(fg, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if class == "WorkerW" || class == "Progman" || class == "Shell_TrayWnd" {
            return false;
        }

        if !IsWindowVisible(fg).as_bool() || IsIconic(fg).as_bool() {
            return false;
        }

        let mut rc_window = RECT::default();
        if GetWindowRect(fg, &mut rc_window).is_err() {
            return false;
        }

        let fg_mon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        let isla_mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if fg_mon != isla_mon {
            // El juego o ventana activa está en otro monitor, no en el de la isla
            return false;
        }

        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(fg_mon, &mut mi).as_bool() {
            return false;
        }

        let rc_mon = mi.rcMonitor;
        // La ventana cubre la pantalla completa si sus límites son iguales o mayores que el monitor
        rc_window.left <= rc_mon.left
            && rc_window.top <= rc_mon.top
            && rc_window.right >= rc_mon.right
            && rc_window.bottom >= rc_mon.bottom
    }
}

/// Interpreta el aviso del AppBar: ¿hay una app a pantalla completa en el monitor de la isla?
pub fn is_active(hwnd: HWND, lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        // Si el AppBar dice que salió, revalidamos con `is_fullscreen` por si sigue habiendo un juego
        is_fullscreen(hwnd)
    } else {
        true
    }
}
