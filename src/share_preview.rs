//! Visor común de recepción: texto, imágenes y archivos con acciones contextuales.
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use windows::core::{w, Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::UI::Controls::{
    SetWindowTheme, DRAWITEMSTRUCT, EM_GETFIRSTVISIBLELINE, EM_GETLINECOUNT, EM_LINESCROLL,
    EM_SETLIMITTEXT, EM_SETRECT, EM_SETSEL, ODS_FOCUS, ODS_HOTLIGHT, ODS_SELECTED,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, ReleaseCapture, SetCapture, SetFocus};
use windows::Win32::UI::Shell::{
    DefSubclassProc, FOLDERID_Downloads, SHGetKnownFolderPath, SetWindowSubclass, ShellExecuteW,
    KF_FLAG_DEFAULT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const HEADER_HEIGHT: i32 = 68;
const BG: COLORREF = COLORREF(0x291b14);
const SURFACE: COLORREF = COLORREF(0x3c2b20);
const CONTENT: COLORREF = COLORREF(0x332319);
const BORDER: COLORREF = COLORREF(0x5b4435);
const TEXT: COLORREF = COLORREF(0xfaf5f2);
const MUTED: COLORREF = COLORREF(0xc9b2a3);
const ACCENT: COLORREF = COLORREF(0xf6b564);
const PRIMARY: COLORREF = COLORREF(0xf2ae58);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContentView {
    Text,
    Image,
    Files,
    Dialog,
}

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
    gallery: Vec<(PathBuf, Image)>,
    current: usize,
    path: Option<&'a Path>,
    view: ContentView,
    confirm: bool,
    pending: usize,
    accepted: bool,
    done: bool,
    scroll_drag: bool,
    scale: f32,
    brush: HBRUSH,
    content_brush: HBRUSH,
    font: HFONT,
    icon: HICON,
}

impl Panel<'_> {
    fn current_image(&self) -> Option<&Image> {
        self.gallery
            .get(self.current)
            .map(|(_, image)| image)
            .or(self.image.as_ref())
    }

    fn current_path(&self) -> Option<&Path> {
        self.gallery
            .get(self.current)
            .map(|(path, _)| path.as_path())
            .or(self.path)
    }

    fn image_count(&self) -> usize {
        if self.gallery.is_empty() {
            usize::from(self.image.is_some())
        } else {
            self.gallery.len()
        }
    }
}

pub fn text_pending(owner: HWND, sender: &str, text: &str, pending: usize) -> Result<bool> {
    show_panel(
        owner,
        "Texto recibido",
        &format!("De {sender} · Hace un momento"),
        Some(text),
        None,
        None,
        Vec::new(),
        pending,
        None,
        None,
    )
}

pub fn image_pending(owner: HWND, sender: &str, path: &Path, pending: usize) -> Result<bool> {
    let image = decode_image(path)?;
    show_panel(
        owner,
        "Imagen recibida",
        &format!("De {sender} · Hace un momento"),
        None,
        Some(image),
        Some(path),
        Vec::new(),
        pending,
        None,
        None,
    )
}

pub fn files_pending(owner: HWND, sender: &str, path: &Path, pending: usize) -> Result<bool> {
    let paths = received_paths(path);
    let gallery = if paths.len() > 1 && paths.iter().all(|path| crate::share::is_image(path)) {
        paths
            .iter()
            .map(|path| decode_image(path).map(|image| (path.clone(), image)))
            .collect::<Result<Vec<_>>>()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if gallery.len() == paths.len() && !gallery.is_empty() {
        return show_panel(
            owner,
            "Imágenes recibidas",
            &format!("De {sender} · Hace un momento"),
            None,
            None,
            Some(path),
            gallery,
            pending,
            None,
            None,
        );
    }
    let entries = file_entries(&paths);
    let title = if entries.len() == 1 {
        "Archivo recibido"
    } else {
        "Archivos recibidos"
    };
    let listing = if entries.is_empty() {
        "No se encontraron los archivos guardados.".to_string()
    } else {
        entries.join("\r\n")
    };
    show_panel(
        owner,
        title,
        &format!("De {sender} · Hace un momento"),
        Some(&listing),
        None,
        Some(path),
        Vec::new(),
        pending,
        None,
        None,
    )
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

pub fn pair(owner: HWND, name: &str, code: &str) -> bool {
    let content = format!("{name}\r\n\r\n{code}\r\n\r\nComprueba que el otro equipo muestre exactamente el mismo código.\r\n\r\nAl vincularlo podrá enviarte texto e imágenes mientras Notch Isla esté abierto. Los archivos seguirán pidiendo confirmación para guardarse.");
    show_with_label(
        owner,
        "Vincular equipo",
        "Confirma el mismo código en ambos equipos.",
        Some(&content),
        None,
        Some("Vincular equipo"),
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
    show_with_label(
        owner,
        title,
        detail,
        text,
        image,
        if confirm {
            Some("Guardar en Descargas")
        } else {
            None
        },
    )
}

fn show_with_label(
    owner: HWND,
    title: &str,
    detail: &str,
    text: Option<&str>,
    image: Option<Image>,
    primary_label: Option<&str>,
) -> Result<bool> {
    show_panel(
        owner,
        title,
        detail,
        text,
        image,
        None,
        Vec::new(),
        0,
        primary_label,
        None,
    )
}

pub fn edit_device_name(owner: HWND, name: &str) -> Result<Option<String>> {
    let mut edited = name.to_owned();
    let saved = show_panel(
        owner,
        "Nombre del equipo",
        "Déjalo vacío para recuperar el nombre original.",
        None,
        None,
        None,
        Vec::new(),
        0,
        Some("Guardar nombre"),
        Some(&mut edited),
    )?;
    Ok(saved.then_some(edited))
}

#[allow(clippy::too_many_arguments)]
fn show_panel(
    owner: HWND,
    title: &str,
    detail: &str,
    text: Option<&str>,
    image: Option<Image>,
    path: Option<&Path>,
    gallery: Vec<(PathBuf, Image)>,
    pending: usize,
    primary_label: Option<&str>,
    edited: Option<&mut String>,
) -> Result<bool> {
    let confirm = primary_label.is_some();
    let editing = edited.is_some();
    let view = if image.is_some() || !gallery.is_empty() {
        ContentView::Image
    } else if path.is_some() {
        ContentView::Files
    } else if !confirm && !editing {
        ContentView::Text
    } else {
        ContentView::Dialog
    };
    let text_lines = text.map_or(0, |text| {
        text.lines()
            .map(|line| 1 + line.chars().count() / 68)
            .sum::<usize>()
    });
    let panel_height = if editing {
        272
    } else {
        match view {
            ContentView::Text => (300 + text_lines.min(11) as i32 * 20).clamp(360, 540),
            ContentView::Image => 620,
            ContentView::Files => (340 + text_lines.min(8) as i32 * 22).clamp(400, 540),
            ContentView::Dialog => 540,
        }
    };
    unsafe {
        let instance = GetModuleHandleW(None)?;
        // MAKEINTRESOURCEW(1): Win32 interpreta la dirección numérica como el ID de
        // assets/nocho.ico, no como una cadena ni como memoria para desreferenciar.
        let resource = PCWSTR(std::ptr::without_provenance::<u16>(1));
        let icon = LoadIconW(Some(instance.into()), resource)?;
        let class = w!("NotchIslaSharePreview");
        let _ = RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: icon,
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
            panel_height,
            Some(owner),
            None,
            Some(instance.into()),
            None,
        )?;
        let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
        let n = |v: i32| (v as f32 * scale).round() as i32;
        let brush = CreateSolidBrush(BG);
        let content_brush = CreateSolidBrush(CONTENT);
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
            gallery,
            current: 0,
            path,
            view,
            confirm,
            pending,
            accepted: false,
            done: false,
            scroll_drag: false,
            scale,
            brush,
            content_brush,
            font,
            icon,
        };
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&mut panel as *mut Panel<'_>) as isize);
        let dark = BOOL(1);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as _,
            std::mem::size_of_val(&dark) as u32,
        );
        let corners = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corners as *const _ as _,
            std::mem::size_of_val(&corners) as u32,
        );
        SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(LPARAM(icon.0 as isize)),
        );
        SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(LPARAM(icon.0 as isize)),
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
        let work_height = monitor.rcWork.bottom - monitor.rcWork.top;
        let height = n(panel_height).min(work_height * 4 / 5);
        SetWindowPos(
            hwnd,
            None,
            monitor.rcWork.left + (monitor.rcWork.right - monitor.rcWork.left - width) / 2,
            monitor.rcWork.top + (monitor.rcWork.bottom - monitor.rcWork.top - height) / 2,
            width,
            height,
            SWP_NOZORDER,
        )?;
        if let Some(text) = edited.as_deref().map(String::as_str).or(text) {
            let style = if editing {
                ES_AUTOHSCROLL
            } else {
                ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL
            };
            let edit = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                &HSTRING::from(text),
                WS_CHILD
                    | WS_VISIBLE
                    | WS_TABSTOP
                    | if editing { WS_BORDER } else { WINDOW_STYLE(0) }
                    | WINDOW_STYLE(style as u32),
                n(if editing { 24 } else { 32 }),
                n(if editing {
                    92 + HEADER_HEIGHT
                } else {
                    100 + HEADER_HEIGHT
                }),
                width - n(if editing { 48 } else { 76 }),
                if editing { n(32) } else { height - n(270) },
                Some(hwnd),
                Some(HMENU(4usize as _)),
                Some(instance.into()),
                None,
            )?;
            if editing {
                SendMessageW(edit, EM_SETLIMITTEXT, Some(WPARAM(96)), None);
                SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            }
            let _ = SetWindowTheme(edit, w!("DarkMode_Explorer"), None);
            if !editing {
                let mut padding = RECT {
                    left: n(8),
                    top: n(6),
                    right: width - n(92),
                    bottom: height - n(282),
                };
                SendMessageW(
                    edit,
                    EM_SETRECT,
                    Some(WPARAM(0)),
                    Some(LPARAM((&mut padding as *mut RECT) as isize)),
                );
                let _ = SetWindowSubclass(edit, Some(edit_proc), 1, hwnd.0 as usize);
            }
            SendMessageW(
                edit,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        let next_label = format!("Siguiente · {}", panel.pending);
        let buttons: Vec<(usize, String, i32, i32)> = match view {
            ContentView::Text if panel.pending > 0 => vec![
                (2, "Cerrar".into(), 24, 96),
                (9, next_label, 132, 180),
                (1, "Copiar".into(), 368, 140),
            ],
            ContentView::Text => vec![
                (2, "Cerrar".into(), 24, 112),
                (1, "Copiar".into(), 368, 140),
            ],
            ContentView::Image if panel.pending > 0 => vec![
                (5, "↓  Guardar".into(), 24, 96),
                (6, "↗  Abrir".into(), 132, 96),
                (9, next_label, 240, 120),
                (1, "▣  Copiar".into(), 368, 140),
            ],
            ContentView::Image => vec![
                (5, "↓  Guardar".into(), 24, 112),
                (6, "↗  Abrir".into(), 148, 112),
                (1, "▣  Copiar".into(), 368, 140),
            ],
            ContentView::Files if panel.pending > 0 => vec![
                (5, "↓  Guardar".into(), 24, 112),
                (9, next_label, 148, 180),
                (6, "↗  Abrir".into(), 396, 112),
            ],
            ContentView::Files => vec![
                (5, "↓  Guardar".into(), 24, 112),
                (6, "↗  Abrir".into(), 396, 112),
            ],
            ContentView::Dialog => vec![
                (1, primary_label.unwrap_or("Aceptar").into(), 280, 228),
                (
                    2,
                    if editing {
                        "Cancelar".into()
                    } else {
                        "Rechazar".into()
                    },
                    24,
                    120,
                ),
            ],
        };
        for (id, label, x, button_width) in buttons {
            let button = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                &HSTRING::from(label),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                n(x),
                height - n(69),
                n(button_width),
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
        if panel.gallery.len() > 1 {
            for (id, label, x) in [(7, "‹", n(32)), (8, "›", width - n(72))] {
                let button = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("BUTTON"),
                    &HSTRING::from(label),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                    x,
                    n(330),
                    n(40),
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
        }
        let close = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("×"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
            width - n(56),
            n(16),
            n(36),
            n(36),
            Some(hwnd),
            Some(HMENU(3usize as _)),
            Some(instance.into()),
            None,
        )?;
        SendMessageW(
            close,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        if editing {
            if let Ok(edit) = GetDlgItem(Some(hwnd), 4) {
                let _ = SetFocus(Some(edit));
            }
        } else if confirm {
            if let Ok(reject) = GetDlgItem(Some(hwnd), 2) {
                let _ = SetFocus(Some(reject));
            }
        }
        // La confirmación vence junto con la solicitud de red.
        if confirm && !editing {
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
            if confirm && msg.message == WM_KEYDOWN && msg.wParam.0 == 13 {
                // Enter solo confirma si el usuario enfocó expresamente la acción principal.
                let focused = GetFocus();
                if GetDlgItem(Some(hwnd), 1).ok() == Some(focused)
                    || (editing && GetDlgItem(Some(hwnd), 4).ok() == Some(focused))
                {
                    panel.accepted = true;
                    panel.done = true;
                } else if GetDlgItem(Some(hwnd), 2).ok() == Some(focused)
                    || GetDlgItem(Some(hwnd), 3).ok() == Some(focused)
                {
                    panel.done = true;
                }
                continue;
            }
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if let Some(edited) = edited.filter(|_| panel.accepted) {
            if let Ok(edit) = GetDlgItem(Some(hwnd), 4) {
                let mut buffer = [0; 97];
                let len = GetWindowTextW(edit, &mut buffer).max(0) as usize;
                *edited = String::from_utf16_lossy(&buffer[..len]);
            } else {
                panel.accepted = false;
            }
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let _ = DestroyWindow(hwnd);
        let _ = DeleteObject(brush.into());
        let _ = DeleteObject(content_brush.into());
        let _ = DeleteObject(font.into());
        if let Some(code) = quit {
            PostQuitMessage(code);
        }
        Ok(panel.accepted)
    }
}

unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    owner: usize,
) -> LRESULT {
    let owner = HWND(owner as _);
    if msg == WM_MOUSEWHEEL {
        let delta = ((wp.0 >> 16) & 0xffff) as u16 as i16;
        let lines = if delta > 0 { -3 } else { 3 };
        SendMessageW(hwnd, EM_LINESCROLL, Some(WPARAM(0)), Some(LPARAM(lines)));
        invalidate_scroll(owner);
        return LRESULT(0);
    }
    let result = DefSubclassProc(hwnd, msg, wp, lp);
    if matches!(msg, WM_KEYDOWN | WM_VSCROLL) {
        invalidate_scroll(owner);
    }
    result
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Retira la barra estándar; la cabecera propia ocupa el área cliente.
    if msg == WM_NCCALCSIZE {
        return LRESULT(0);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Panel<'_>;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let panel = &mut *ptr;
    let n = |v: i32| (v as f32 * panel.scale).round() as i32;
    match msg {
        WM_NCHITTEST => {
            let mut rect = RECT::default();
            let _ = GetWindowRect(hwnd, &mut rect);
            let x = (lp.0 as u32 & 0xffff) as i16 as i32 - rect.left;
            let y = ((lp.0 as u32 >> 16) & 0xffff) as i16 as i32 - rect.top;
            if y >= 0 && y < n(HEADER_HEIGHT) && x >= 0 && x < rect.right - rect.left - n(64) {
                LRESULT(HTCAPTION as isize)
            } else {
                LRESULT(HTCLIENT as isize)
            }
        }
        WM_CLOSE => {
            panel.done = true;
            LRESULT(0)
        }
        WM_LBUTTONDOWN if matches!(panel.view, ContentView::Text | ContentView::Files) => {
            let (x, y) = client_point(lp);
            let content = content_rect(hwnd, panel);
            let n = |value: i32| (value as f32 * panel.scale).round() as i32;
            if x >= content.right - n(18)
                && x <= content.right
                && y >= content.top
                && y <= content.bottom
                && scroll_to_pointer(hwnd, panel, &content, y)
            {
                panel.scroll_drag = true;
                let _ = SetCapture(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE if panel.scroll_drag => {
            let (_, y) = client_point(lp);
            let content = content_rect(hwnd, panel);
            let _ = scroll_to_pointer(hwnd, panel, &content, y);
            LRESULT(0)
        }
        WM_LBUTTONUP if panel.scroll_drag => {
            panel.scroll_drag = false;
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            panel.scroll_drag = false;
            LRESULT(0)
        }
        WM_TIMER => {
            if wp.0 == 1 && panel.confirm {
                panel.done = true;
            } else if wp.0 == 2 {
                let _ = KillTimer(Some(hwnd), 2);
                if let Ok(button) = GetDlgItem(Some(hwnd), 1) {
                    let label = if panel.view == ContentView::Image {
                        "▣  Copiar"
                    } else {
                        "Copiar"
                    };
                    let _ = SetWindowTextW(button, &HSTRING::from(label));
                    let _ = InvalidateRect(Some(button), None, true);
                }
                if let Ok(button) = GetDlgItem(Some(hwnd), 5) {
                    let _ = SetWindowTextW(button, w!("↓  Guardar"));
                    let _ = InvalidateRect(Some(button), None, true);
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            match wp.0 & 0xffff {
                1 if panel.confirm => {
                    panel.accepted = true;
                    panel.done = true;
                }
                1 => {
                    let result = if let Some(image) = panel.current_image() {
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
                    SetTimer(Some(hwnd), 2, 1600, None);
                }
                5 => {
                    let result = panel.current_path().map(save_received).transpose();
                    let saved = matches!(result, Ok(Some(_)));
                    if let Ok(button) = GetDlgItem(Some(hwnd), 5) {
                        let label = if saved { "✓  Guardado" } else { "Reintentar" };
                        let _ = SetWindowTextW(button, &HSTRING::from(label));
                        let _ = InvalidateRect(Some(button), None, true);
                    }
                    SetTimer(Some(hwnd), 2, 1600, None);
                }
                6 => {
                    if let Some(path) = panel.current_path() {
                        let _ = open_path(hwnd, &open_target(path, panel.view));
                    }
                }
                7 if !panel.gallery.is_empty() => {
                    panel.current = panel
                        .current
                        .checked_sub(1)
                        .unwrap_or(panel.gallery.len() - 1);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                8 if !panel.gallery.is_empty() => {
                    panel.current = (panel.current + 1) % panel.gallery.len();
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                9 if panel.pending > 0 => {
                    panel.accepted = true;
                    panel.done = true;
                }
                2 | 3 => panel.done = true,
                _ => {}
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT => {
            let dc = HDC(wp.0 as _);
            SetTextColor(dc, TEXT);
            SetBkColor(dc, CONTENT);
            LRESULT(panel.content_brush.0 as isize)
        }
        WM_DRAWITEM => {
            let item = &*(lp.0 as *const DRAWITEMSTRUCT);
            let primary = item.CtlID == 1 || (panel.view == ContentView::Files && item.CtlID == 6);
            let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
            let hovered = item.itemState.0 & ODS_HOTLIGHT.0 != 0;
            let brush = CreateSolidBrush(if primary {
                if pressed || hovered {
                    ACCENT
                } else {
                    PRIMARY
                }
            } else if item.CtlID == 3 {
                BG
            } else if pressed || hovered {
                BORDER
            } else {
                SURFACE
            });
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
            SetTextColor(item.hDC, if primary { BG } else { TEXT });
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
            let scroll_paint = scroll_paint_rect(hwnd, panel.scale);
            if matches!(panel.view, ContentView::Text | ContentView::Files)
                && ps.rcPaint.left >= scroll_paint.left
                && ps.rcPaint.right <= scroll_paint.right
            {
                FillRect(dc, &ps.rcPaint, panel.content_brush);
                let content = content_rect(hwnd, panel);
                paint_scroll_indicator(hwnd, dc, panel, &content);
                let _ = EndPaint(hwnd, &ps);
                return LRESULT(0);
            }
            FillRect(dc, &client, panel.brush);
            let border = CreateSolidBrush(BORDER);
            let _ = FrameRect(dc, &client, border);
            let _ = DeleteObject(border.into());
            SetBkMode(dc, TRANSPARENT);
            paint_header(dc, panel, &client);
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
            for (text, top, color) in [(panel.title, 22, TEXT), (panel.detail, 54, MUTED)] {
                SelectObject(dc, if top == 22 { heading } else { panel.font }.into());
                SetTextColor(dc, color);
                let mut rect = RECT {
                    left: n(24),
                    top: n(top + HEADER_HEIGHT),
                    right: client.right - n(24),
                    bottom: n(top + HEADER_HEIGHT + 26),
                };
                let mut text: Vec<u16> = text.encode_utf16().collect();
                DrawTextW(
                    dc,
                    &mut text,
                    &mut rect,
                    DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                );
            }
            if panel.image_count() > 1 {
                SelectObject(dc, panel.font.into());
                SetTextColor(dc, MUTED);
                let mut counter: Vec<u16> =
                    format!("{} de {}", panel.current + 1, panel.image_count())
                        .encode_utf16()
                        .collect();
                let mut rect = RECT {
                    left: client.right - n(120),
                    top: n(90),
                    right: client.right - n(24),
                    bottom: n(118),
                };
                DrawTextW(
                    dc,
                    &mut counter,
                    &mut rect,
                    DT_RIGHT | DT_SINGLELINE | DT_NOPREFIX,
                );
            }
            SelectObject(dc, panel.font.into());
            let _ = DeleteObject(heading.into());
            let content_rect = RECT {
                left: n(24),
                top: n(154),
                right: client.right - n(24),
                bottom: client.bottom - n(92),
            };
            if panel.view != ContentView::Dialog {
                let fill = CreateSolidBrush(CONTENT);
                let old_brush = SelectObject(dc, fill.into());
                let pen = CreatePen(PS_SOLID, 1, BORDER);
                let old_pen = SelectObject(dc, pen.into());
                let _ = RoundRect(
                    dc,
                    content_rect.left,
                    content_rect.top,
                    content_rect.right,
                    content_rect.bottom,
                    n(14),
                    n(14),
                );
                SelectObject(dc, old_brush);
                SelectObject(dc, old_pen);
                let _ = DeleteObject(fill.into());
                let _ = DeleteObject(pen.into());
            }
            if matches!(panel.view, ContentView::Text | ContentView::Files) {
                paint_scroll_indicator(hwnd, dc, panel, &content_rect);
            }
            if let Some(image) = panel.current_image() {
                let box_rect = content_rect;
                let aw = (box_rect.right - box_rect.left - n(32)).max(1);
                let ah = (box_rect.bottom - box_rect.top - n(32)).max(1);
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
                    box_rect.left + n(16) + (aw - w) / 2,
                    box_rect.top + n(16) + (ah - h) / 2,
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

fn client_point(lp: LPARAM) -> (i32, i32) {
    (
        (lp.0 as u32 & 0xffff) as i16 as i32,
        ((lp.0 as u32 >> 16) & 0xffff) as i16 as i32,
    )
}

unsafe fn content_rect(hwnd: HWND, panel: &Panel<'_>) -> RECT {
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let n = |value: i32| (value as f32 * panel.scale).round() as i32;
    RECT {
        left: n(24),
        top: n(154),
        right: client.right - n(24),
        bottom: client.bottom - n(92),
    }
}

unsafe fn scroll_paint_rect(hwnd: HWND, scale: f32) -> RECT {
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let n = |value: i32| (value as f32 * scale).round() as i32;
    RECT {
        left: client.right - n(40),
        top: n(164),
        right: client.right - n(26),
        bottom: client.bottom - n(98),
    }
}

unsafe fn invalidate_scroll(hwnd: HWND) {
    let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
    let rect = scroll_paint_rect(hwnd, scale);
    let _ = InvalidateRect(Some(hwnd), Some(&rect), false);
}

unsafe fn line_scroll_state(hwnd: HWND, panel: &Panel<'_>) -> Option<(HWND, i32, i32, i32)> {
    let edit = GetDlgItem(Some(hwnd), 4).ok()?;
    let total = SendMessageW(edit, EM_GETLINECOUNT, None, None).0 as i32;
    let first = SendMessageW(edit, EM_GETFIRSTVISIBLELINE, None, None).0 as i32;
    let mut edit_rect = RECT::default();
    GetClientRect(edit, &mut edit_rect).ok()?;
    let line_height = (20.0 * panel.scale).round().max(1.0) as i32;
    let visible = ((edit_rect.bottom - edit_rect.top) / line_height).max(1);
    (total > visible).then_some((edit, total, visible, first))
}

unsafe fn scroll_to_pointer(hwnd: HWND, panel: &Panel<'_>, content: &RECT, pointer_y: i32) -> bool {
    let Some((edit, total, visible, first)) = line_scroll_state(hwnd, panel) else {
        return false;
    };
    let n = |value: i32| (value as f32 * panel.scale).round() as i32;
    let track_top = content.top + n(12);
    let track_height = (content.bottom - n(12) - track_top).max(1);
    let thumb_height = ((track_height as f32 * visible as f32 / total as f32).round() as i32)
        .clamp(n(24), track_height);
    let travel = (track_height - thumb_height).max(1);
    let ratio = ((pointer_y - track_top - thumb_height / 2) as f32 / travel as f32).clamp(0.0, 1.0);
    let target = ((total - visible) as f32 * ratio).round() as i32;
    SendMessageW(
        edit,
        EM_LINESCROLL,
        Some(WPARAM(0)),
        Some(LPARAM((target - first) as isize)),
    );
    invalidate_scroll(hwnd);
    true
}

unsafe fn paint_scroll_indicator(hwnd: HWND, dc: HDC, panel: &Panel<'_>, content: &RECT) {
    let Some((_, total, visible, first)) = line_scroll_state(hwnd, panel) else {
        return;
    };
    let n = |value: i32| (value as f32 * panel.scale).round() as i32;
    let track_top = content.top + n(12);
    let track_bottom = content.bottom - n(12);
    let track_height = (track_bottom - track_top).max(1);
    let thumb_height = ((track_height as f32 * visible as f32 / total as f32).round() as i32)
        .clamp(n(24), track_height);
    let range = (total - visible).max(1) as f32;
    let progress = first.max(0) as f32 / range;
    let thumb_top = track_top + ((track_height - thumb_height) as f32 * progress).round() as i32;
    let track = CreateSolidBrush(COLORREF(0x463b32));
    let previous_brush = SelectObject(dc, track.into());
    let previous_pen = SelectObject(dc, GetStockObject(NULL_PEN));
    let _ = RoundRect(
        dc,
        content.right - n(11),
        track_top,
        content.right - n(5),
        track_bottom,
        n(5),
        n(5),
    );
    SelectObject(dc, previous_brush);
    let _ = DeleteObject(track.into());
    let brush = CreateSolidBrush(COLORREF(0x806f5f));
    let previous_brush = SelectObject(dc, brush.into());
    let _ = RoundRect(
        dc,
        content.right - n(11),
        thumb_top,
        content.right - n(5),
        thumb_top + thumb_height,
        n(5),
        n(5),
    );
    SelectObject(dc, previous_brush);
    SelectObject(dc, previous_pen);
    let _ = DeleteObject(brush.into());
}

/// Marca, subtítulo espaciado y una línea de acento discreta.
unsafe fn paint_header(dc: HDC, panel: &Panel<'_>, client: &RECT) {
    let n = |v: i32| (v as f32 * panel.scale).round() as i32;
    let _ = DrawIconEx(
        dc,
        n(24),
        n(18),
        panel.icon,
        n(32),
        n(32),
        0,
        None,
        DI_NORMAL,
    );
    for (label, size, weight, top, color, spacing) in [
        ("Notch Isla", 18, 600, 12, TEXT, 0),
        ("COMPARTIR CERCA", 10, 500, 38, ACCENT, 1),
    ] {
        let font = CreateFontW(
            -n(size),
            0,
            0,
            0,
            weight,
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
        let previous = SelectObject(dc, font.into());
        let previous_spacing = SetTextCharacterExtra(dc, n(spacing));
        SetTextColor(dc, color);
        let mut rect = RECT {
            left: n(70),
            top: n(top),
            right: client.right - n(72),
            bottom: n(top + size + 6),
        };
        let mut text: Vec<u16> = label.encode_utf16().collect();
        DrawTextW(
            dc,
            &mut text,
            &mut rect,
            DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        SetTextCharacterExtra(dc, previous_spacing);
        SelectObject(dc, previous);
        let _ = DeleteObject(font.into());
    }
    let separator = CreateSolidBrush(BORDER);
    FillRect(
        dc,
        &RECT {
            left: n(24),
            top: n(HEADER_HEIGHT - 1),
            right: client.right - n(24),
            bottom: n(HEADER_HEIGHT),
        },
        separator,
    );
    let _ = DeleteObject(separator.into());
}

fn decode_image(path: &Path) -> Result<Image> {
    unsafe {
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
        Ok(Image {
            pixels,
            width,
            height,
        })
    }
}

fn received_paths(path: &Path) -> Vec<PathBuf> {
    let mut paths = if path.is_dir() {
        std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect::<Vec<_>>()
    } else {
        vec![path.to_owned()]
    };
    paths.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    paths
}

fn file_entries(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .take(100)
        .map(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Archivo".into());
            let name = name
                .get(5..)
                .filter(|_| {
                    name.as_bytes()
                        .get(0..2)
                        .is_some_and(|prefix| prefix.iter().all(u8::is_ascii_digit))
                        && name.as_bytes().get(2..5) == Some(b" - ")
                })
                .unwrap_or(&name);
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_uppercase)
                .unwrap_or_else(|| "ARCHIVO".into());
            let size = path.metadata().map_or(0, |metadata| metadata.len());
            format!("▣  {name}   ·   {extension}   ·   {}", format_size(size))
        })
        .collect()
}

fn open_target(path: &Path, view: ContentView) -> PathBuf {
    if view == ContentView::Files && path.is_dir() {
        let paths = received_paths(path);
        if paths.len() == 1 {
            return paths[0].clone();
        }
    }
    path.to_owned()
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.1} GB", bytes / GB)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes / KB)
    } else {
        format!("{bytes:.0} B")
    }
}

fn save_received(source: &Path) -> Result<PathBuf> {
    if source.is_dir() {
        // Los archivos normales ya pasaron por el flujo de confirmación y están en Descargas.
        return Ok(source.to_owned());
    }
    let downloads = unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None)?;
        let path = PathBuf::from(OsString::from_wide(raw.as_wide()));
        CoTaskMemFree(Some(raw.0.cast()));
        path
    };
    let extension = source.extension().and_then(|extension| extension.to_str());
    let mut destination = downloads.join(match extension {
        Some(extension) => format!("Imagen recibida.{extension}"),
        None => "Imagen recibida".to_string(),
    });
    for copy in 2..=999 {
        if !destination.exists() {
            break;
        }
        destination = downloads.join(match extension {
            Some(extension) => format!("Imagen recibida ({copy}).{extension}"),
            None => format!("Imagen recibida ({copy})"),
        });
    }
    std::fs::copy(source, &destination).map_err(|error| {
        windows::core::Error::new(E_FAIL, format!("No se pudo guardar: {error}"))
    })?;
    Ok(destination)
}

fn open_path(owner: HWND, path: &Path) -> Result<()> {
    let path = HSTRING::from(path.as_os_str());
    let launched =
        unsafe { ShellExecuteW(Some(owner), w!("open"), &path, None, None, SW_SHOWNORMAL) };
    if launched.0 as isize > 32 {
        Ok(())
    } else {
        Err(windows::core::Error::from(E_FAIL))
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
    fn presenta_archivos_con_nombre_original_y_tamano_legible() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("01 - Documento largo.pdf");
        std::fs::write(&path, vec![0; 1536]).unwrap();
        let entries = file_entries(&received_paths(directory.path()));
        assert_eq!(entries.len(), 1);
        assert!(entries[0].contains("Documento largo.pdf"));
        assert!(!entries[0].contains("01 -"));
        assert!(entries[0].contains("PDF"));
        assert!(entries[0].contains("1.5 KB"));
        assert_eq!(open_target(directory.path(), ContentView::Files), path);
    }

    #[test]
    fn formatea_tamanos_sin_perder_unidades() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    #[ignore = "diálogo interactivo de vinculación para revisión visual"]
    fn revisar_vinculacion() {
        pair(HWND::default(), "Mi portátil", "A12B · C34D · E56F");
    }

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
        text_pending(HWND::default(), "Mi portátil", "Hola 👋\r\nEste es el texto recibido desde otro equipo.\r\n\r\nPuedes seleccionarlo o copiar todo con el botón Copiar.", 0)?;
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
        image_pending(HWND::default(), "Mi portátil", &path, 0)?;
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
