//! Ícono de la app que reproduce la música (Spotify, Chrome...), para la insignia de la carátula.
//!
//! SMTC solo da un identificador de la app (`SourceAppUserModelId`). Puede ser un AUMID
//! ("SpotifyAB.SpotifyMusic_...!Spotify", "Chrome", "MSEdge") o el nombre del .exe
//! ("Spotify.exe"). Se prueba primero la carpeta virtual de apps del shell, que entiende los
//! AUMID, y si no, se busca el proceso en ejecución y se usa el ícono de su .exe.

use tiny_skia::{IntSize, Pixmap};
use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::{CloseHandle, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY,
};

/// Lado en píxeles del ícono pedido: alcanza para la insignia hasta 300 % de escala.
const ICON_PX: i32 = 64;

/// Ícono de la app con identificador `app_id`, o `None` si no se encuentra.
pub fn load(app_id: &str) -> Option<Pixmap> {
    // Este código corre en hilos de eventos de WinRT: COM ya suele estar listo (MTA);
    // si no, se inicializa. Si el hilo tenía otro modo, la llamada falla sin efecto.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    from_shell(&format!("shell:AppsFolder\\{app_id}"), ICON_PX)
        .or_else(|| exe_path(app_id).and_then(|path| from_shell(&path, ICON_PX)))
}

/// Logo de Nocho (el ícono de este ejecutable) a `px` píxeles de lado, para dibujarlo en las
/// ventanas propias. Se llama desde el hilo de la interfaz, que ya tiene COM listo.
pub fn own(px: i32) -> Option<Pixmap> {
    let exe = std::env::current_exe().ok()?;
    from_shell(exe.to_str()?, px)
}

/// Pide al shell el ícono de un elemento (una app de la carpeta de apps o un .exe).
fn from_shell(name: &str, px: i32) -> Option<Pixmap> {
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(name), None).ok()?;
        let bitmap = factory
            .GetImage(SIZE { cx: px, cy: px }, SIIGBF_ICONONLY)
            .ok()?;
        let pixmap = to_pixmap(bitmap);
        let _ = DeleteObject(bitmap.into());
        pixmap
    }
}

/// Copia un HBITMAP de 32 bits a un Pixmap (RGBA premultiplicado).
fn to_pixmap(bitmap: HBITMAP) -> Option<Pixmap> {
    unsafe {
        let mut info = BITMAP::default();
        let read = GetObjectW(
            bitmap.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut info as *mut BITMAP as *mut _),
        );
        if read == 0 || info.bmWidth <= 0 || info.bmHeight == 0 {
            return None;
        }
        let (w, h) = (info.bmWidth, info.bmHeight.abs());

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // filas de arriba hacia abajo
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut data = vec![0u8; (w * h * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            h as u32,
            Some(data.as_mut_ptr().cast()),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        if lines == 0 {
            return None;
        }

        // BGRA → RGBA. Sin canal alfa (todo en 0) el ícono es opaco. Si algún color supera a
        // su alfa, los datos vienen sin premultiplicar y se premultiplican para tiny-skia.
        let has_alpha = data.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
        let straight = data
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]);
        for p in data.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
            if !has_alpha {
                p[3] = 255;
            } else if straight {
                let a = p[3] as u16;
                for c in &mut p[..3] {
                    *c = ((*c as u16 * a + 127) / 255) as u8;
                }
            }
        }
        Pixmap::from_vec(data, IntSize::from_wh(w as u32, h as u32)?)
    }
}

/// Ruta completa del .exe de un proceso en ejecución cuyo nombre coincide con `app_id`
/// (con o sin ".exe"; por ejemplo "Spotify.exe" o "Chrome").
fn exe_path(app_id: &str) -> Option<String> {
    let wanted = if app_id.to_lowercase().ends_with(".exe") {
        app_id.to_lowercase()
    } else {
        format!("{}.exe", app_id.to_lowercase())
    };
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut pid = None;
        let mut ok = Process32FirstW(snapshot, &mut entry).is_ok();
        while ok {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase() == wanted {
                pid = Some(entry.th32ProcessID);
                break;
            }
            ok = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid?).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(process);
        result.ok()?;
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }
}
