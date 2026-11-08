//! Ventana de recepción: contenido temporal y copia explícita al portapapeles.
use std::path::Path;
use windows::core::{w, Result, BOOL, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::UI::Controls::{SetWindowTheme, DRAWITEMSTRUCT, ODS_FOCUS};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

struct Image {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}
struct Panel<'a> {
    title: &'a str,
    detail: &'a str,
    text: Option<&'a str>,
    image: Option<Image>,
    confirm: bool,
    accepted: bool,
    done: bool,
    scale: f32,
    brush: HBRUSH,
    font: HFONT,
}

pub fn text(owner: HWND, sender: &str, text: &str) -> Result<()> {
    show(
        owner,
        "Texto recibido",
        &format!("De {sender} · Copia el contenido cuando quieras."),
        Some(text),
        None,
        false,
    )
    .map(|_| ())
}

pub fn image(owner: HWND, sender: &str, path: &Path) -> Result<()> {
    let image = unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let decoder = factory.CreateDecoderFromFilename(
            &HSTRING::from(path.as_os_str()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnLoad,
        )?;
        let frame = decoder.GetFrame(0)?;
        let (mut width, mut height) = (0, 0);
        frame.GetSize(&mut width, &mut height)?;
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 40_000_000 {
            return Err(windows::core::Error::from(E_INVALIDARG));
        }
        let converter = factory.CreateFormatConverter()?;
        converter.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let mut pixels = vec![0; width as usize * height as usize * 4];
        converter.CopyPixels(std::ptr::null(), width * 4, &mut pixels)?;
        Image {
            pixels,
            width,
            height,
        }
    };
    show(
        owner,
        "Imagen recibida",
        &format!("De {sender} · Copia la imagen para pegarla."),
        None,
        Some(image),
        false,
    )
    .map(|_| ())
}

pub fn confirm(owner: HWND, description: &str, code: &str) -> bool {
    let detail = format!("Código de este equipo · {code}");
    show(
        owner,
        "Archivos entrantes",
        &detail,
        Some(description),
        None,
        true,
    )
    .unwrap_or(false)
}

fn show(
    owner: HWND,
    title: &str,
    detail: &str,
    text: Option<&str>,
    image: Option<Image>,
    confirm: bool,
) -> Result<bool> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("NotchIslaSharePreview");
        let _ = RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        });
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class,
            w!("Compartir cerca · Notch Isla"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            560,
            500,
            Some(owner),
            None,
            Some(instance.into()),
            None,
        )?;
        let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
        let n = |v: i32| (v as f32 * scale).round() as i32;
        let brush = CreateSolidBrush(COLORREF(0x191919));
        let font = CreateFontW(
            -n(15),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        );
        let mut panel = Panel {
            title,
            detail,
            text,
            image,
            confirm,
            accepted: false,
            done: false,
            scale,
            brush,
            font,
        };
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&mut panel as *mut Panel<'_>) as isize);
        let dark = BOOL(1);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as _,
            std::mem::size_of_val(&dark) as u32,
        );
        let mut monitor = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(
            MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST),
            &mut monitor,
        );
        let width = n(560).min(monitor.rcWork.right - monitor.rcWork.left);
        let height = n(500).min(monitor.rcWork.bottom - monitor.rcWork.top);
        SetWindowPos(
            hwnd,
            None,
            monitor.rcWork.left + (monitor.rcWork.right - monitor.rcWork.left - width) / 2,
            monitor.rcWork.top + (monitor.rcWork.bottom - monitor.rcWork.top - height) / 2,
            width,
            height,
            SWP_NOZORDER,
        )?;
        if let Some(text) = text {
            let edit = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                &HSTRING::from(text),
                WS_CHILD
                    | WS_VISIBLE
                    | WS_TABSTOP
                    | WS_VSCROLL
                    | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32),
                n(24),
                n(92),
                width - n(64),
                height - n(218),
                Some(hwnd),
                None,
                Some(instance.into()),
                None,
            )?;
            let _ = SetWindowTheme(edit, w!("DarkMode_Explorer"), None);
            SendMessageW(
                edit,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        for (id, label, x) in [
            (
                1,
                if confirm {
                    "Guardar en Descargas"
                } else {
                    "Copiar"
                },
                280,
            ),
            (2, if confirm { "Rechazar" } else { "Cerrar" }, 24),
        ] {
            let button = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                &HSTRING::from(label),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                n(x),
                height - n(101),
                n(if id == 1 { 228 } else { 120 }),
                n(40),
                Some(hwnd),
                Some(HMENU(id as _)),
                Some(instance.into()),
                None,
            )?;
            SendMessageW(
                button,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        // La confirmación vence junto con la solicitud de red.
        if confirm {
            SetTimer(Some(hwnd), 1, 60_000, None);
        }
        let mut msg = MSG::default();
        let mut quit = None;
        while !panel.done {
            let status = GetMessageW(&mut msg, None, 0, 0).0;
            if status <= 0 {
                if status == 0 {
                    quit = Some(msg.wParam.0 as i32);
                }
                break;
            }
            if msg.message == WM_KEYDOWN && msg.wParam.0 == 27 {
                break;
            }
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let _ = DestroyWindow(hwnd);
        let _ = DeleteObject(brush.into());
        let _ = DeleteObject(font.into());
        if let Some(code) = quit {
            PostQuitMessage(code);
        }
        Ok(panel.accepted)
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Panel<'_>;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let panel = &mut *ptr;
    let n = |v: i32| (v as f32 * panel.scale).round() as i32;
    match msg {
        WM_CLOSE | WM_TIMER => {
            panel.done = true;
            LRESULT(0)
        }
        WM_COMMAND => {
            match wp.0 & 0xffff {
                1 if panel.confirm => {
                    panel.accepted = true;
                    panel.done = true;
                }
                1 => {
                    let result = if let Some(image) = &panel.image {
                        copy_image(hwnd, image)
                    } else {
                        let bytes: Vec<u8> = panel
                            .text
                            .unwrap_or_default()
                            .encode_utf16()
                            .chain(Some(0))
                            .flat_map(u16::to_le_bytes)
                            .collect();
                        copy_bytes(hwnd, 13, &bytes)
                    };
                    let label = HSTRING::from(if result.is_ok() {
                        "Copiado ✓"
                    } else {
                        "Reintentar copiar"
                    });
                    if let Ok(button) = GetDlgItem(Some(hwnd), 1) {
                        let _ = SetWindowTextW(button, &label);
                        let _ = InvalidateRect(Some(button), None, true);
                    }
                }
                2 => panel.done = true,
                _ => {}
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT => {
            let dc = HDC(wp.0 as _);
            SetTextColor(dc, COLORREF(0xf0f0f0));
            SetBkColor(dc, COLORREF(0x191919));
            LRESULT(panel.brush.0 as isize)
        }
        WM_DRAWITEM => {
            let item = &*(lp.0 as *const DRAWITEMSTRUCT);
            let primary = item.CtlID == 1;
            let brush = CreateSolidBrush(COLORREF(if primary { 0xf2b56b } else { 0x333333 }));
            FillRect(item.hDC, &item.rcItem, panel.brush);
            let old_brush = SelectObject(item.hDC, brush.into());
            let old_pen = SelectObject(item.hDC, GetStockObject(NULL_PEN));
            let _ = RoundRect(
                item.hDC,
                item.rcItem.left,
                item.rcItem.top,
                item.rcItem.right,
                item.rcItem.bottom,
                n(16),
                n(16),
            );
            SelectObject(item.hDC, old_brush);
            SelectObject(item.hDC, old_pen);
            let _ = DeleteObject(brush.into());
            SetBkMode(item.hDC, TRANSPARENT);
            SetTextColor(
                item.hDC,
                COLORREF(if primary { 0x151515 } else { 0xf5f5f5 }),
            );
            let old = SelectObject(item.hDC, panel.font.into());
            let mut label = [0u16; 128];
            let len = GetWindowTextW(item.hwndItem, &mut label);
            let mut rect = item.rcItem;
            DrawTextW(
                item.hDC,
                &mut label[..len as usize],
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            if item.itemState.0 & ODS_FOCUS.0 != 0 {
                let _ = DrawFocusRect(item.hDC, &rect);
            }
            SelectObject(item.hDC, old);
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            FillRect(dc, &client, panel.brush);
            SetBkMode(dc, TRANSPARENT);
            SelectObject(dc, panel.font.into());
            let heading = CreateFontW(
                -n(22),
                0,
                0,
                0,
                600,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            );
            for (text, top, color) in [(panel.title, 22, 0xffffff), (panel.detail, 54, 0xb0b0b0)] {
                SelectObject(dc, if top == 22 { heading } else { panel.font }.into());
                SetTextColor(dc, COLORREF(color));
                let mut rect = RECT {
                    left: n(24),
                    top: n(top),
                    right: client.right - n(24),
                    bottom: n(top + 26),
                };
                let mut text: Vec<u16> = text.encode_utf16().collect();
                DrawTextW(
                    dc,
                    &mut text,
                    &mut rect,
                    DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                );
            }
            SelectObject(dc, panel.font.into());
            let _ = DeleteObject(heading.into());
            if let Some(image) = &panel.image {
                let aw = (client.right - n(48)).max(1);
                let ah = (client.bottom - n(180)).max(1);
                let scale = (aw as f32 / image.width as f32)
                    .min(ah as f32 / image.height as f32)
                    .min(1.0);
                let (w, h) = (
                    (image.width as f32 * scale) as i32,
                    (image.height as f32 * scale) as i32,
                );
                let info = bitmap_info(image);
                SetStretchBltMode(dc, HALFTONE);
                StretchDIBits(
                    dc,
                    (client.right - w) / 2,
                    n(92) + (ah - h) / 2,
                    w,
                    h,
                    0,
                    0,
                    image.width as i32,
                    image.height as i32,
                    Some(image.pixels.as_ptr().cast()),
                    &info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn bitmap_info(image: &Image) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: 40,
            biWidth: image.width as i32,
            biHeight: -(image.height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

unsafe fn copy_image(hwnd: HWND, image: &Image) -> Result<()> {
    let info = bitmap_info(image);
    let header = std::slice::from_raw_parts(
        (&info.bmiHeader as *const BITMAPINFOHEADER).cast::<u8>(),
        40,
    );
    let mut dib = header.to_vec();
    dib.extend_from_slice(&image.pixels);
    copy_bytes(hwnd, 8, &dib)
}

unsafe fn copy_bytes(hwnd: HWND, format: u32, bytes: &[u8]) -> Result<()> {
    let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
    let ptr = GlobalLock(memory);
    if ptr.is_null() {
        let _ = GlobalFree(Some(memory));
        return Err(windows::core::Error::from_thread());
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast(), bytes.len());
    let _ = GlobalUnlock(memory);
    let result = (|| {
        OpenClipboard(Some(hwnd))?;
        let result = (|| {
            EmptyClipboard()?;
            SetClipboardData(format, Some(HANDLE(memory.0)))?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    })();
    if result.is_err() {
        let _ = GlobalFree(Some(memory));
    }
    result
}

pub fn clipboard_image(hwnd: HWND) -> Option<Vec<u8>> {
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let result = (|| {
            let memory = HGLOBAL(GetClipboardData(8).ok()?.0);
            let size = GlobalSize(memory);
            if !(40..=crate::share::MAX_IMAGE as usize - 14).contains(&size) {
                return None;
            }
            let ptr = GlobalLock(memory).cast::<u8>();
            if ptr.is_null() {
                return None;
            }
            let dib = std::slice::from_raw_parts(ptr, size).to_vec();
            let _ = GlobalUnlock(memory);
            let header = u32::from_le_bytes(dib[0..4].try_into().ok()?) as usize;
            if header < 40 || header > size {
                return None;
            }
            let bits = u16::from_le_bytes(dib[14..16].try_into().ok()?);
            let compression = u32::from_le_bytes(dib[16..20].try_into().ok()?);
            let colors = u32::from_le_bytes(dib[32..36].try_into().ok()?) as usize;
            let palette = if colors != 0 {
                colors
            } else if bits <= 8 {
                1usize << bits
            } else {
                0
            };
            let offset = 14
                + header
                + palette.checked_mul(4)?
                + if header == 40 && compression == 3 {
                    12
                } else {
                    0
                };
            if offset > size + 14 {
                return None;
            }
            let mut bmp = b"BM".to_vec();
            bmp.extend_from_slice(&((size + 14) as u32).to_le_bytes());
            bmp.extend_from_slice(&[0; 4]);
            bmp.extend_from_slice(&(offset as u32).to_le_bytes());
            bmp.extend_from_slice(&dib);
            Some(bmp)
        })();
        let _ = CloseClipboard();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "vista previa interactiva para revisión visual"]
    fn revisar_ventanas_de_share() -> Result<()> {
        unsafe {
            windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            )
            .ok()?;
        }
        text(HWND::default(), "Mi portátil", "Hola 👋\r\nEste es el texto recibido desde otro equipo.\r\n\r\nPuedes seleccionarlo o copiar todo con el botón Copiar.")?;
        let mut pixels = Vec::new();
        for y in 0..180 {
            for x in 0..320 {
                pixels.extend_from_slice(&[180, (y * 255 / 180) as u8, (x * 255 / 320) as u8, 255]);
            }
        }
        let sample = Image {
            pixels,
            width: 320,
            height: 180,
        };
        let info = bitmap_info(&sample);
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&((54 + sample.pixels.len()) as u32).to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        unsafe {
            bmp.extend_from_slice(std::slice::from_raw_parts(
                (&info.bmiHeader as *const BITMAPINFOHEADER).cast::<u8>(),
                40,
            ));
        }
        bmp.extend_from_slice(&sample.pixels);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.bmp");
        std::fs::write(&path, bmp).unwrap();
        image(HWND::default(), "Mi portátil", &path)?;
        confirm(
            HWND::default(),
            "Mi portátil quiere enviarte 2 archivos.\r\n\r\nDocumento.pdf\r\nProyecto.zip",
            "ABCDEF123456",
        );
        unsafe {
            windows::Win32::System::Com::CoUninitialize();
        }
        Ok(())
    }
}
