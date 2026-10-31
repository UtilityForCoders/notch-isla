//! Pantallas conectadas, para elegir en cuál va la isla.

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromPoint, DEVMODEW,
    ENUM_CURRENT_SETTINGS, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTOPRIMARY,
};

/// `MONITORINFOF_PRIMARY`: la pantalla principal.
const PRIMARY: u32 = 1;

pub struct Monitor {
    pub handle: HMONITOR,
    /// Nombre de dispositivo ("\\.\DISPLAY2"): se guarda en los ajustes.
    pub device: String,
    /// Nombre para el menú ("Pantalla 2 (principal)").
    pub label: String,
}

pub fn list() -> Vec<Monitor> {
    unsafe extern "system" fn add(handle: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        list.push(handle);
        true.into()
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(add),
            LPARAM(&mut handles as *mut _ as isize),
        );
    }
    handles
        .into_iter()
        .filter_map(|handle| {
            let mut info = MONITORINFOEXW {
                monitorInfo: MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                    ..Default::default()
                },
                ..Default::default()
            };
            unsafe { GetMonitorInfoW(handle, &mut info.monitorInfo).ok().ok()? };
            let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(32);
            let device = String::from_utf16_lossy(&info.szDevice[..len]);
            // "\\.\DISPLAY2" -> "Pantalla 2".
            let number = device.trim_start_matches(|c: char| !c.is_ascii_digit());
            let mut label = format!("Pantalla {number}");
            if info.monitorInfo.dwFlags & PRIMARY != 0 {
                label.push_str(" (principal)");
            }
            Some(Monitor {
                handle,
                device,
                label,
            })
        })
        .collect()
}

/// La pantalla guardada en los ajustes; si no está conectada (o no hay), la principal.
pub fn find(device: &str) -> HMONITOR {
    if !device.is_empty() {
        if let Some(m) = list().into_iter().find(|m| m.device == device) {
            return m.handle;
        }
    }
    unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }
}

/// Frecuencia de refresco de la pantalla en Hz (por ejemplo 60, 120, 144, 240).
/// Si no se puede determinar, devuelve 60.
pub fn refresh_rate(handle: HMONITOR) -> u32 {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        if !GetMonitorInfoW(handle, &mut info.monitorInfo).as_bool() {
            return 60;
        }
        let mut devmode = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        if EnumDisplaySettingsW(
            windows::core::PCWSTR(info.szDevice.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut devmode,
        )
        .as_bool()
        {
            if devmode.dmDisplayFrequency > 1 {
                return devmode.dmDisplayFrequency;
            }
        }
    }
    60
}

#[link(name = "dwmapi")]
extern "system" {
    pub fn DwmFlush() -> windows::core::HRESULT;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consulta_frecuencia_de_refresco() {
        let monitor = find("");
        let hz = refresh_rate(monitor);
        println!("Monitor Hz: {hz}");
        assert!(hz >= 30 && hz <= 1000);
        unsafe {
            let start = std::time::Instant::now();
            for _ in 0..10 {
                let _ = DwmFlush();
            }
            let avg_ms = start.elapsed().as_secs_f64() * 1000.0 / 10.0;
            println!("DwmFlush avg ms per frame: {avg_ms:.2} ms ({:.1} fps)", 1000.0 / avg_ms);
        }
    }
}
