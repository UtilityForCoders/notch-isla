//! Menú con el estilo de la isla (fondo negro redondeado, Segoe UI, resaltado suave), en lugar
//! del menú clásico de Windows. Lo usan el ícono de la bandeja y el clic derecho sobre la isla.
//!
//! Funciona como `TrackPopupMenu`: `show` abre una ventana layered junto al cursor, atiende
//! mensajes hasta que se elige una opción o se cierra, y devuelve la elegida. Los grupos
//! ("Ocultarse sola tras", "Temporizador"...) se despliegan dentro del mismo menú.
//!
//! Se cierra con Esc, al elegir una opción, al hacer clic fuera (un gancho de mouse que solo
//! existe mientras el menú está abierto) o si otra ventana pasa al frente.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::Instant;

use tiny_skia::{Color, Paint, PathBuilder};
use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, D2D1_ANTIALIAS_MODE_ALIASED,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_WEIGHT_REGULAR, DWRITE_TEXT_METRICS,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT,
    VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, GetWindowRect, KillTimer, LoadCursorW, PostMessageW, PostQuitMessage,
    RegisterClassExW, SetForegroundWindow, SetTimer, SetWindowsHookExW, ShowWindow,
    TranslateMessage, UnhookWindowsHookEx, UpdateLayeredWindow, IDC_ARROW, MSG, MSLLHOOKSTRUCT,
    SW_SHOWNOACTIVATE, ULW_ALPHA, WA_INACTIVE, WH_MOUSE_LL, WM_ACTIVATE, WM_CLOSE, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows_numerics::Vector2;

use crate::island::{fill_rounded_rect, rounded_rect_path, stroke, stroke_path};
use crate::text::make_format;
use crate::Surface;

/// Una opción del menú.
pub enum Item {
    /// Opción que se puede elegir: `show` devuelve su `id`. `checked` dibuja la marca.
    Action {
        id: usize,
        label: String,
        checked: bool,
    },
    /// Texto gris que no se elige (un título o una explicación).
    Label(String),
    Separator,
    /// Grupo que se despliega dentro del mismo menú (sus opciones van con sangría).
    Group {
        label: String,
        items: Vec<Item>,
    },
}

// Medidas en píxeles lógicos.
const PAD: f32 = 6.0;
const ROW_H: f32 = 30.0;
const LABEL_H: f32 = 26.0;
const SEPARATOR_H: f32 = 9.0;
const TEXT_X: f32 = 12.0;
const CHILD_INDENT: f32 = 14.0;
/// Espacio a la derecha para la marca o la flecha del grupo.
const RIGHT: f32 = 30.0;
const RADIUS: f32 = 14.0;
const ROW_RADIUS: f32 = 8.0;
const MIN_W: f32 = 190.0;
const MAX_W: f32 = 360.0;
const FONT: f32 = 13.0;
const LABEL_FONT: f32 = 12.0;
/// Entrada: aparece desvaneciéndose en este tiempo.
const FADE_SECS: f32 = 0.12;
const FADE_TIMER: usize = 1;
const FRAME_MS: u32 = 15;

/// Hay un menú abierto: otro clic en la bandeja mientras tanto no abre uno encima.
static OPEN: AtomicBool = AtomicBool::new(false);
/// Ventana del menú abierto, para el gancho de mouse.
static MENU_HWND: AtomicIsize = AtomicIsize::new(0);

/// Acción que debe tomar el menú tras pulsar una opción.
pub enum MenuAction {
    /// Cierra el menú (la opción ya se procesó).
    Close,
    /// Mantiene el menú abierto y actualiza las opciones (por ejemplo, alternar casillas de verificación).
    KeepOpen(Vec<Item>),
}

thread_local! {
    static HANDLER: std::cell::Cell<Option<(*mut (), unsafe fn(*mut (), usize) -> MenuAction)>> = const { std::cell::Cell::new(None) };
}

/// Abre el menú junto al cursor y espera. Devuelve el `id` elegido, o `None` si se cerró sin
/// elegir (o si no se pudo crear: entonces no pasa nada más).
pub fn show(items: Vec<Item>) -> Option<usize> {
    let mut chosen = None;
    show_interactive(items, |id| {
        chosen = Some(id);
        MenuAction::Close
    });
    chosen
}

/// Abre el menú interactivo, permitiendo procesar opciones y mantenerlo abierto con nuevos items.
pub fn show_interactive<F>(items: Vec<Item>, mut on_action: F)
where
    F: FnMut(usize) -> MenuAction,
{
    if OPEN.swap(true, Ordering::SeqCst) {
        return;
    }
    unsafe fn call_handler<F: FnMut(usize) -> MenuAction>(ptr: *mut (), id: usize) -> MenuAction {
        unsafe { (*(ptr as *mut F))(id) }
    }
    let ptr = &mut on_action as *mut F as *mut ();
    HANDLER.set(Some((ptr, call_handler::<F>)));
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            HANDLER.set(None);
            OPEN.store(false, Ordering::SeqCst);
        }
    }
    let _guard = Reset;
    let _ = run(items);
}

/// Opción ya maquetada.
enum Entry {
    Action {
        id: usize,
        text: usize,
        checked: bool,
    },
    Label {
        text: usize,
    },
    Separator,
    Group {
        text: usize,
        children: Vec<Entry>,
    },
}

/// Texto maquetado por DirectWrite (medidas en píxeles físicos).
struct Text {
    layout: IDWriteTextLayout,
    w: f32,
    h: f32,
}

#[derive(Clone, Copy)]
enum RowKind {
    Action {
        id: usize,
        checked: bool,
    },
    Label,
    Separator,
    /// `index` = posición del grupo entre las opciones de primer nivel.
    Group {
        index: usize,
        open: bool,
    },
}

impl RowKind {
    fn selectable(self) -> bool {
        matches!(self, RowKind::Action { .. } | RowKind::Group { .. })
    }
}

/// Una fila visible: posición y alto en píxeles lógicos.
struct Row {
    kind: RowKind,
    text: Option<usize>,
    child: bool,
    y: f32,
    h: f32,
}

struct State {
    hwnd: HWND,
    entries: Vec<Entry>,
    texts: Vec<Text>,
    /// Grupos desplegados (por posición entre las opciones de primer nivel).
    open: Vec<bool>,
    rows: Vec<Row>,
    hover: Option<usize>,
    tracking: bool,
    scale: f32,
    /// Ancho lógico: fijo, el que necesita el texto más largo (también el de los grupos).
    width: f32,
    /// Punto del cursor al abrir y si el menú crece hacia arriba (no cabía hacia abajo).
    anchor: POINT,
    up: Option<bool>,
    work: RECT,
    surface: Surface,
    pos: POINT,
    target: ID2D1DCRenderTarget,
    opened: Instant,
    /// `Some` al terminar: la opción elegida o `None` si se cerró.
    done: Option<Option<usize>>,
    dwrite: IDWriteFactory,
    font: IDWriteTextFormat,
    label_font: IDWriteTextFormat,
}

impl State {
    fn update_items(&mut self, items: Vec<Item>) {
        let mut texts = Vec::new();
        let entries = build(&self.dwrite, &self.font, &self.label_font, items, &mut texts);
        let width = text_width(&entries, &texts, self.scale, false).clamp(MIN_W, MAX_W);
        self.entries = entries;
        self.texts = texts;
        self.width = self.width.max(width);
        if self.open.len() < self.entries.len() {
            self.open.resize(self.entries.len(), false);
        }
        relayout(self);
        render(self);
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state(f: impl FnOnce(&mut State)) {
    STATE.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            if let Some(state) = guard.as_mut() {
                f(state);
            }
        }
    });
}

fn run(items: Vec<Item>) -> Result<Option<usize>> {
    unsafe {
        let mut anchor = POINT::default();
        GetCursorPos(&mut anchor)?;
        let monitor = MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dpi, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
        let scale = dpi as f32 / 96.0;

        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let font = make_format(&dwrite, DWRITE_FONT_WEIGHT_REGULAR, FONT * scale)?;
        let label_font = make_format(&dwrite, DWRITE_FONT_WEIGHT_REGULAR, LABEL_FONT * scale)?;
        let mut texts = Vec::new();
        let entries = build(&dwrite, &font, &label_font, items, &mut texts);
        let width = text_width(&entries, &texts, scale, false).clamp(MIN_W, MAX_W);

        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let target = factory.CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        })?;
        target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);

        let hwnd = create_window()?;
        let open = vec![false; entries.len()];
        STATE.with(|cell| {
            *cell.borrow_mut() = Some(State {
                hwnd,
                entries,
                texts,
                open,
                rows: Vec::new(),
                hover: None,
                tracking: false,
                scale,
                width,
                anchor,
                up: None,
                work: info.rcWork,
                // Se reemplaza en `relayout` con el tamaño real.
                surface: Surface::new(1, 1).expect("superficie mínima"),
                pos: POINT::default(),
                target,
                opened: Instant::now(),
                done: None,
                dwrite,
                font,
                label_font,
            })
        });
        with_state(|st| {
            relayout(st);
            render(st);
        });
        MENU_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0).ok();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        // Al frente para recibir el teclado y saber si otra ventana pasa adelante.
        let _ = SetForegroundWindow(hwnd);
        SetTimer(Some(hwnd), FADE_TIMER, FRAME_MS, None);

        // Bucle propio hasta que el menú termine; la isla sigue recibiendo sus mensajes.
        let mut msg = MSG::default();
        loop {
            let finished = STATE.with(|c| c.borrow().as_ref().is_none_or(|s| s.done.is_some()));
            if finished {
                break;
            }
            let got = GetMessageW(&mut msg, None, 0, 0);
            if got.0 == 0 {
                // Llegó WM_QUIT (la app se cierra): se devuelve a la cola para el bucle principal.
                PostQuitMessage(msg.wParam.0 as i32);
                break;
            }
            if got.0 == -1 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if let Some(hook) = hook {
            let _ = UnhookWindowsHookEx(hook);
        }
        MENU_HWND.store(0, Ordering::SeqCst);
        let state = STATE.with(|c| c.borrow_mut().take());
        let _ = DestroyWindow(hwnd);
        Ok(state.and_then(|s| s.done).flatten())
    }
}

fn create_window() -> Result<HWND> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("NotchIslaMenu");
        if !REGISTERED.swap(true, Ordering::SeqCst) {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&wc);
        }
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("Menú de Notch Isla"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

/// Convierte las opciones en entradas con su texto maquetado.
fn build(
    dwrite: &IDWriteFactory,
    font: &IDWriteTextFormat,
    label_font: &IDWriteTextFormat,
    items: Vec<Item>,
    texts: &mut Vec<Text>,
) -> Vec<Entry> {
    let mut add = |text: &str, format: &IDWriteTextFormat| -> usize {
        if let Some(t) = layout(dwrite, format, text) {
            texts.push(t);
        } else if let Some(t) = layout(dwrite, format, " ") {
            texts.push(t);
        }
        texts.len().saturating_sub(1)
    };
    let mut out = Vec::new();
    for item in items {
        out.push(match item {
            Item::Action { id, label, checked } => Entry::Action {
                id,
                text: add(&label, font),
                checked,
            },
            Item::Label(label) => Entry::Label {
                text: add(&label, label_font),
            },
            Item::Separator => Entry::Separator,
            Item::Group { label, items } => {
                let text = add(&label, font);
                let mut children = Vec::new();
                for child in items {
                    children.push(match child {
                        Item::Action { id, label, checked } => Entry::Action {
                            id,
                            text: add(&label, font),
                            checked,
                        },
                        Item::Label(label) => Entry::Label {
                            text: add(&label, label_font),
                        },
                        // Dentro de un grupo solo van opciones y textos.
                        _ => continue,
                    });
                }
                Entry::Group { text, children }
            }
        });
    }
    out
}

fn layout(dwrite: &IDWriteFactory, format: &IDWriteTextFormat, text: &str) -> Option<Text> {
    let wide: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let layout = dwrite
            .CreateTextLayout(&wide, format, f32::MAX, f32::MAX)
            .ok()?;
        let mut m = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut m).ok()?;
        Some(Text {
            layout,
            w: m.width,
            h: m.height,
        })
    }
}

/// Ancho lógico que necesita el texto más largo, contando sangrías y márgenes.
fn text_width(entries: &[Entry], texts: &[Text], scale: f32, child: bool) -> f32 {
    let indent = if child { CHILD_INDENT } else { 0.0 };
    entries
        .iter()
        .map(|e| match e {
            Entry::Action { text, .. } | Entry::Label { text } => {
                texts[*text].w / scale + indent + 2.0 * (PAD + TEXT_X) + RIGHT
            }
            Entry::Separator => 0.0,
            Entry::Group { text, children } => {
                (texts[*text].w / scale + 2.0 * (PAD + TEXT_X) + RIGHT)
                    .max(text_width(children, texts, scale, true))
            }
        })
        .fold(0.0, f32::max)
}

/// Rehace las filas visibles y el tamaño y la posición de la ventana.
fn relayout(st: &mut State) {
    let mut rows = Vec::new();
    let mut y = PAD;
    let mut push = |kind: RowKind, text: Option<usize>, child: bool, rows: &mut Vec<Row>| {
        let h = match kind {
            RowKind::Separator => SEPARATOR_H,
            RowKind::Label => LABEL_H,
            _ => ROW_H,
        };
        rows.push(Row {
            kind,
            text,
            child,
            y,
            h,
        });
        y += h;
    };
    for (index, e) in st.entries.iter().enumerate() {
        match e {
            Entry::Action { id, text, checked } => push(
                RowKind::Action {
                    id: *id,
                    checked: *checked,
                },
                Some(*text),
                false,
                &mut rows,
            ),
            Entry::Label { text } => push(RowKind::Label, Some(*text), false, &mut rows),
            Entry::Separator => push(RowKind::Separator, None, false, &mut rows),
            Entry::Group { text, children } => {
                let open = st.open[index];
                push(
                    RowKind::Group { index, open },
                    Some(*text),
                    false,
                    &mut rows,
                );
                if open {
                    for c in children {
                        match c {
                            Entry::Action { id, text, checked } => push(
                                RowKind::Action {
                                    id: *id,
                                    checked: *checked,
                                },
                                Some(*text),
                                true,
                                &mut rows,
                            ),
                            Entry::Label { text } => {
                                push(RowKind::Label, Some(*text), true, &mut rows)
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    let height = y + PAD;
    st.rows = rows;

    let s = st.scale;
    let w = (st.width * s).ceil() as i32;
    let h = (height * s).ceil() as i32;
    let work = st.work;
    // Hacia la derecha del cursor si cabe; si no, hacia la izquierda.
    let x = if st.anchor.x + w <= work.right {
        st.anchor.x
    } else {
        st.anchor.x - w
    }
    .max(work.left);
    // La dirección se decide al abrir (hacia abajo si cabe) y se mantiene al desplegar grupos.
    let up = *st.up.get_or_insert(st.anchor.y + h > work.bottom);
    let y = if up {
        (st.anchor.y - h).max(work.top)
    } else {
        st.anchor.y.min(work.bottom - h).max(work.top)
    };
    st.pos = POINT { x, y };
    if st.surface.width != w as u32 || st.surface.height != h as u32 {
        if let Ok(surface) = Surface::new(w.max(1) as u32, h.max(1) as u32) {
            st.surface = surface;
        }
    }
}

fn render(st: &mut State) {
    let s = st.scale;
    let w = st.surface.width as f32;
    let h = st.surface.height as f32;
    let pixmap = &mut st.surface.pixmap;
    pixmap.fill(Color::TRANSPARENT);
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    // Cuerpo gris oscuro (más claro que la isla, para que se lea mejor), con un borde tenue
    // para separarlo de fondos oscuros.
    paint.set_color_rgba8(44, 44, 48, 250);
    fill_rounded_rect(pixmap, &paint, 0.0, 0.0, w, h, RADIUS * s);
    paint.set_color_rgba8(255, 255, 255, 34);
    if let Some(border) = rounded_rect_path(0.5, 0.5, w - 1.0, h - 1.0, RADIUS * s - 0.5) {
        stroke_path(pixmap, &paint, &border, 1.0);
    }

    for (i, row) in st.rows.iter().enumerate() {
        let (y0, rh) = (row.y * s, row.h * s);
        let cy = y0 + rh / 2.0;
        match row.kind {
            RowKind::Separator => {
                paint.set_color_rgba8(255, 255, 255, 30);
                let x = (PAD + TEXT_X) * s;
                fill_rounded_rect(pixmap, &paint, x, cy - 0.5, w - 2.0 * x, 1.0, 0.0);
            }
            kind => {
                if st.hover == Some(i) && kind.selectable() {
                    paint.set_color_rgba8(255, 255, 255, 30);
                    fill_rounded_rect(
                        pixmap,
                        &paint,
                        PAD * s,
                        y0,
                        w - 2.0 * PAD * s,
                        rh,
                        ROW_RADIUS * s,
                    );
                }
                let cx = w - (PAD + RIGHT / 2.0) * s;
                let mut pb = PathBuilder::new();
                match kind {
                    RowKind::Action { checked: true, .. } => {
                        paint.set_color_rgba8(255, 255, 255, 255);
                        pb.move_to(cx - 4.5 * s, cy);
                        pb.line_to(cx - 1.5 * s, cy + 3.0 * s);
                        pb.line_to(cx + 4.5 * s, cy - 3.5 * s);
                    }
                    RowKind::Group { open, .. } => {
                        paint.set_color_rgba8(255, 255, 255, 150);
                        if open {
                            pb.move_to(cx - 4.0 * s, cy - 2.0 * s);
                            pb.line_to(cx, cy + 2.0 * s);
                            pb.line_to(cx + 4.0 * s, cy - 2.0 * s);
                        } else {
                            pb.move_to(cx - 2.0 * s, cy - 4.0 * s);
                            pb.line_to(cx + 2.0 * s, cy);
                            pb.line_to(cx - 2.0 * s, cy + 4.0 * s);
                        }
                    }
                    _ => {}
                }
                stroke(pixmap, &paint, pb, 1.6 * s);
            }
        }
    }
    st.surface.copy_to_dib();

    // Textos con DirectWrite, encima del dibujo.
    let full = RECT {
        left: 0,
        top: 0,
        right: st.surface.width as i32,
        bottom: st.surface.height as i32,
    };
    unsafe {
        if st.target.BindDC(st.surface.dc, &full).is_ok() {
            st.target.BeginDraw();
            for row in &st.rows {
                let Some(text) = row.text.and_then(|t| st.texts.get(t)) else {
                    continue;
                };
                let indent = if row.child { CHILD_INDENT } else { 0.0 };
                let x = (PAD + TEXT_X + indent) * s;
                let y = row.y * s + (row.h * s - text.h) / 2.0;
                let gray = matches!(row.kind, RowKind::Label);
                let c = if gray { 0.6 } else { 1.0 };
                let color = D2D1_COLOR_F {
                    r: c,
                    g: c,
                    b: c + if gray { 0.03 } else { 0.0 },
                    a: 1.0,
                };
                let clip = D2D_RECT_F {
                    left: x,
                    top: row.y * s,
                    right: w - (PAD + RIGHT) * s,
                    bottom: (row.y + row.h) * s,
                };
                if let Ok(brush) = st.target.CreateSolidColorBrush(&color, None) {
                    st.target
                        .PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
                    st.target.DrawTextLayout(
                        Vector2 { X: x, Y: y },
                        &text.layout,
                        &brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                    st.target.PopAxisAlignedClip();
                }
            }
            let _ = st.target.EndDraw(None, None);
        }

        let t = st.opened.elapsed().as_secs_f32() / FADE_SECS;
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (t.clamp(0.0, 1.0) * 255.0) as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let size = SIZE {
            cx: st.surface.width as i32,
            cy: st.surface.height as i32,
        };
        let _ = UpdateLayeredWindow(
            st.hwnd,
            None,
            Some(&st.pos),
            Some(&size),
            Some(st.surface.dc),
            Some(&POINT::default()),
            windows::Win32::Foundation::COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
    }
}

/// Fila seleccionable bajo el punto `y` (píxeles físicos de la ventana).
fn row_at(st: &State, y: f32) -> Option<usize> {
    let y = y / st.scale;
    st.rows
        .iter()
        .position(|r| y >= r.y && y < r.y + r.h)
        .filter(|&i| st.rows[i].kind.selectable())
}

/// Elige la fila `i`: una opción termina el menú o lo mantiene abierto según `MenuAction`; un grupo se despliega o se pliega.
fn activate(st: &mut State, i: usize) {
    match st.rows.get(i).map(|r| r.kind) {
        Some(RowKind::Action { id, .. }) => {
            let action = HANDLER.with(|cell| {
                if let Some((ptr, invoke)) = cell.get() {
                    unsafe { invoke(ptr, id) }
                } else {
                    MenuAction::Close
                }
            });
            match action {
                MenuAction::Close => {
                    st.done = Some(Some(id));
                }
                MenuAction::KeepOpen(items) => {
                    st.update_items(items);
                }
            }
        }
        Some(RowKind::Group { index, .. }) => {
            st.open[index] = !st.open[index];
            relayout(st);
            render(st);
        }
        _ => {}
    }
}

/// Mueve la selección con el teclado a la siguiente fila seleccionable (`dir` = 1 o -1).
fn step(st: &mut State, dir: i32) {
    let n = st.rows.len() as i32;
    if n == 0 {
        return;
    }
    let mut i = st.hover.map_or(if dir > 0 { -1 } else { n }, |h| h as i32);
    for _ in 0..n {
        i = (i + dir).rem_euclid(n);
        if st.rows[i as usize].kind.selectable() {
            st.hover = Some(i as usize);
            render(st);
            return;
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
            with_state(|st| {
                if !st.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    st.tracking = true;
                }
                let hover = row_at(st, y);
                if hover != st.hover {
                    st.hover = hover;
                    render(st);
                }
            });
            LRESULT(0)
        }
        // WM_MOUSELEAVE
        0x02A3 => {
            with_state(|st| {
                st.tracking = false;
                if st.hover.is_some() {
                    st.hover = None;
                    render(st);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
            with_state(|st| {
                if let Some(i) = row_at(st, y) {
                    activate(st, i);
                }
            });
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = wparam.0 as u16;
            with_state(|st| match key {
                k if k == VK_ESCAPE.0 => st.done = Some(None),
                k if k == VK_DOWN.0 => step(st, 1),
                k if k == VK_UP.0 => step(st, -1),
                k if k == VK_RETURN.0 || k == VK_SPACE.0 => {
                    if let Some(i) = st.hover {
                        activate(st, i);
                    }
                }
                // Derecha despliega el grupo seleccionado; izquierda lo pliega.
                k if k == VK_RIGHT.0 || k == VK_LEFT.0 => {
                    if let Some(RowKind::Group { open, .. }) =
                        st.hover.and_then(|i| st.rows.get(i)).map(|r| r.kind)
                    {
                        if open != (k == VK_RIGHT.0) {
                            if let Some(i) = st.hover {
                                activate(st, i);
                            }
                        }
                    }
                }
                _ => {}
            });
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == FADE_TIMER => {
            with_state(|st| {
                render(st);
                if st.opened.elapsed().as_secs_f32() >= FADE_SECS {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), FADE_TIMER);
                    }
                }
            });
            LRESULT(0)
        }
        // Otra ventana pasó al frente (Alt+Tab, clic en otra app): se cierra.
        WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            with_state(|st| st.done = Some(None));
            LRESULT(0)
        }
        // Lo manda el gancho de mouse al hacer clic fuera.
        WM_CLOSE => {
            with_state(|st| st.done = Some(None));
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Gancho de mouse de todo el sistema, solo mientras el menú está abierto: un clic fuera de
/// él lo cierra (el clic sigue su curso normal).
unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let down = matches!(
        wparam.0 as u32,
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN
    );
    let menu = MENU_HWND.load(Ordering::SeqCst);
    if code >= 0 && down && menu != 0 {
        let hwnd = HWND(menu as _);
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(hwnd, &mut rect);
        }
        let p = info.pt;
        let inside = p.x >= rect.left && p.x < rect.right && p.y >= rect.top && p.y < rect.bottom;
        if !inside {
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
