//! Menú con el estilo de la isla (fondo negro redondeado, Segoe UI, resaltado suave), en lugar
//! del menú clásico de Windows. Lo usan el ícono de la bandeja y el clic derecho sobre la isla.
//!
//! Funciona como `TrackPopupMenu`: `show` abre una ventana layered junto al cursor, atiende
//! mensajes hasta que se elige una opción o se cierra, y devuelve la elegida. Los grupos
//! ("Pantalla", "Temporizador"...) abren submenús laterales sin agrandar el menú principal.
//!
//! Se cierra con Esc, al elegir una opción, al hacer clic fuera (un gancho de mouse que solo
//! existe mientras el menú está abierto) o si otra ventana pasa al frente.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;

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
    GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION,
    MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN,
    VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    FindWindowExW, FindWindowW, GetAncestor, GetClassNameW, GetCursorPos, GetForegroundWindow,
    GetMessageW, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, KillTimer, LoadCursorW,
    PostMessageW, PostQuitMessage, RegisterClassExW, SetForegroundWindow, SetTimer,
    SetWindowsHookExW, ShowWindow, TranslateMessage, UnhookWindowsHookEx, UpdateLayeredWindow,
    WindowFromPoint, GA_ROOT, GA_ROOTOWNER, IDC_ARROW, KBDLLHOOKSTRUCT, MA_NOACTIVATE, MSG,
    MSLLHOOKSTRUCT, SW_SHOWNOACTIVATE, ULW_ALPHA, WA_INACTIVE, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_ACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_CLOSE, WM_DESTROY, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows_numerics::Vector2;

use crate::island::{fill_rounded_rect, rounded_rect_path, stroke, stroke_path};
use crate::text::make_format;
use crate::Surface;

/// Una opción del menú.
#[derive(Clone, PartialEq)]
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
    /// Grupo con un submenú lateral independiente.
    Group {
        label: String,
        items: Vec<Item>,
    },
    /// Dispositivo informativo: conserva el estado y los íconos visibles en una sola fila.
    DeviceStatus {
        name: String,
        status: String,
        icon: crate::island::NoticeIcon,
        connection: crate::island::NoticeIcon,
    },
    /// Deslizador continuo; el comando emitido es `id + value` (ambos límites incluidos).
    Slider {
        id: usize,
        label: String,
        value: u32,
        min: u32,
        max: u32,
    },
}

// Medidas en píxeles lógicos.
const PAD: f32 = 6.0;
const ROW_H: f32 = 30.0;
const LABEL_H: f32 = 26.0;
const SLIDER_H: f32 = 56.0;
const SEPARATOR_H: f32 = 9.0;
const TEXT_X: f32 = 12.0;
const CHILD_INDENT: f32 = 14.0;
/// Espacio a la derecha para la marca o la flecha del grupo.
const RIGHT: f32 = 30.0;
const DEVICE_ICON_SPACE: f32 = 24.0;
const DEVICE_STATUS_GAP: f32 = 10.0;
const RADIUS: f32 = 14.0;
const ROW_RADIUS: f32 = 8.0;
const MIN_W: f32 = 190.0;
const MAX_W: f32 = 360.0;
const FONT: f32 = 13.0;
const LABEL_FONT: f32 = 12.0;
const FOCUS_TIMER: usize = 2;
const SLIDER_TIMER: usize = 3;
const SUBMENU_TIMER: usize = 4;
const WM_SLIDER_MOVE: u32 = WM_APP + 1;

/// Hay un menú abierto: otro clic en la bandeja mientras tanto no abre uno encima.
static OPEN: AtomicBool = AtomicBool::new(false);
/// Ventana del menú abierto, para el gancho de mouse.
static MENU_HWND: AtomicIsize = AtomicIsize::new(0);
static MENU_FOCUS: AtomicIsize = AtomicIsize::new(0);
static MENU_MOUSE_HOOK: AtomicBool = AtomicBool::new(false);
/// Panel que recibe el arrastre por el gancho mientras Explorer conserva el foco.
static SLIDER_HWND: AtomicIsize = AtomicIsize::new(0);
static MENU_KEYBOARD: AtomicIsize = AtomicIsize::new(0);
static PANELS: Mutex<Vec<isize>> = Mutex::new(Vec::new());

/// Acción que debe tomar el menú tras pulsar una opción.
pub enum MenuAction {
    /// Cierra el menú (la opción ya se procesó).
    Close,
    /// Mantiene el menú abierto y actualiza las opciones (por ejemplo, alternar casillas de verificación).
    // Se conserva para los menús interactivos y sus pruebas de regresión.
    #[cfg_attr(not(test), allow(dead_code))]
    KeepOpen(Vec<Item>),
    /// Cambia solo el deslizador y una casilla relacionada, conservando los demás layouts.
    #[cfg_attr(not(test), allow(dead_code))]
    UpdateSlider {
        id: usize,
        value: u32,
        label: String,
        checked: Option<(usize, bool)>,
    },
}

type MenuCallback = (*mut (), unsafe fn(*mut (), usize) -> MenuAction);

thread_local! {
    static HANDLER: std::cell::Cell<Option<MenuCallback>> = const { std::cell::Cell::new(None) };
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
    show_with_handler(items, &mut on_action, false);
}

/// El menú usa un dueño propio y estable, independiente de la isla y de Explorer.
pub fn show_tray<F>(items: Vec<Item>, mut on_action: F)
where
    F: FnMut(usize) -> MenuAction,
{
    show_with_handler(items, &mut on_action, true);
}

fn show_with_handler<F>(items: Vec<Item>, mut on_action: F, from_tray: bool)
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
    let _ = run(items, from_tray, true);
}

/// Opción ya maquetada.
enum Entry {
    Slider {
        id: usize,
        text: usize,
        value: u32,
        min: u32,
        max: u32,
    },
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
    },
    DeviceStatus {
        text: usize,
        status: usize,
        icon: crate::island::NoticeIcon,
        connection: crate::island::NoticeIcon,
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
    Slider {
        id: usize,
        value: u32,
        min: u32,
        max: u32,
    },
    Action {
        id: usize,
        checked: bool,
    },
    Label,
    DeviceStatus {
        status: usize,
        icon: crate::island::NoticeIcon,
        connection: crate::island::NoticeIcon,
    },
    Separator,
    /// `index` = posición del grupo entre las opciones de primer nivel.
    Group {
        index: usize,
        open: bool,
    },
}

impl RowKind {
    fn selectable(self) -> bool {
        matches!(
            self,
            RowKind::Action { .. } | RowKind::Group { .. } | RowKind::Slider { .. }
        )
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
    child_menu: Option<Box<State>>,
    parent_bounds: Option<RECT>,
    prefer_left: bool,
    pending_action: Option<MenuAction>,
    items: Vec<Item>,
    scroll: f32,
    content_height: f32,
    keyboard_pointer: Option<POINT>,
    entries: Vec<Entry>,
    texts: Vec<Text>,
    /// Grupos desplegados (por posición entre las opciones de primer nivel).
    open: Vec<bool>,
    rows: Vec<Row>,
    hover: Option<usize>,
    tracking: bool,
    dragging_slider: Option<usize>,
    pending_slider: Option<f32>,
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
    /// `Some` al terminar: la opción elegida o `None` si se cerró.
    done: Option<Option<usize>>,
    dwrite: IDWriteFactory,
    font: IDWriteTextFormat,
    label_font: IDWriteTextFormat,
}

impl State {
    fn update_slider_item(
        &mut self,
        id: usize,
        value: u32,
        label: &str,
        checked: Option<(usize, bool)>,
    ) {
        fn sync(
            items: &mut [Item],
            id: usize,
            value: u32,
            label: &str,
            checked: Option<(usize, bool)>,
        ) {
            for item in items {
                match item {
                    Item::Slider {
                        id: candidate,
                        value: current,
                        label: text,
                        ..
                    } if *candidate == id => {
                        *current = value;
                        *text = label.into();
                    }
                    Item::Action {
                        id: candidate,
                        checked: current,
                        ..
                    } => {
                        if let Some((wanted, next)) = checked {
                            if *candidate == wanted {
                                *current = next;
                            }
                        }
                    }
                    Item::Group { items, .. } => sync(items, id, value, label, checked),
                    _ => {}
                }
            }
        }
        sync(&mut self.items, id, value, label, checked);
        fn update(
            entries: &mut [Entry],
            id: usize,
            value: u32,
            checked: Option<(usize, bool)>,
        ) -> Option<usize> {
            let mut text_index = None;
            for entry in entries {
                match entry {
                    Entry::Slider {
                        id: item_id,
                        value: current,
                        min,
                        max,
                        text,
                    } if *item_id == id => {
                        *current = value.clamp(*min, *max);
                        text_index = Some(*text);
                    }
                    Entry::Action {
                        id,
                        checked: current,
                        ..
                    } => {
                        if let Some((action, next)) = checked {
                            if *id == action {
                                *current = next;
                            }
                        }
                    }
                    _ => {}
                }
            }
            text_index
        }
        let text_index = update(&mut self.entries, id, value, checked);
        if let Some(index) = text_index {
            if let Some(text) = layout(&self.dwrite, &self.font, label) {
                self.texts[index] = text;
            }
        }
        let mut changed = text_index.is_some();
        for row in &mut self.rows {
            match &mut row.kind {
                RowKind::Slider {
                    id: item_id,
                    value: current,
                    min,
                    max,
                } if *item_id == id => {
                    *current = value.clamp(*min, *max);
                }
                RowKind::Action {
                    id,
                    checked: current,
                } => {
                    if let Some((action, next)) = checked {
                        if *id == action && *current != next {
                            *current = next;
                            changed = true;
                        }
                    }
                }
                _ => {}
            }
        }
        if changed {
            let width =
                text_width(&self.entries, &self.texts, self.scale, false).clamp(MIN_W, MAX_W);
            if width > self.width {
                self.width = width;
                relayout(self);
            }
            render(self);
        }
        if let Some(child) = &mut self.child_menu {
            child.update_slider_item(id, value, label, checked);
        }
    }

    fn update_items(&mut self, items: Vec<Item>) {
        let mut texts = Vec::new();
        let entries = build(
            &self.dwrite,
            &self.font,
            &self.label_font,
            &items,
            &mut texts,
        );
        let width = text_width(&entries, &texts, self.scale, false).clamp(MIN_W, MAX_W);
        self.entries = entries;
        self.texts = texts;
        self.items = items;
        self.width = self.width.max(width);
        if self.open.len() < self.entries.len() {
            self.open.resize(self.entries.len(), false);
        }
        relayout(self);
        render(self);
        refresh_submenu(self);
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if self.parent_bounds.is_some() {
            let _ = SLIDER_HWND.compare_exchange(
                self.hwnd.0 as isize,
                0,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            PANELS
                .lock()
                .unwrap()
                .retain(|id| *id != self.hwnd.0 as isize);
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

fn panel_mut(state: &mut State, hwnd: HWND) -> Option<&mut State> {
    if state.hwnd == hwnd {
        Some(state)
    } else {
        panel_mut(state.child_menu.as_deref_mut()?, hwnd)
    }
}

fn pending_action(state: &mut State) -> Option<MenuAction> {
    state
        .child_menu
        .as_deref_mut()
        .and_then(pending_action)
        .or_else(|| state.pending_action.take())
}

fn settle_submenus(state: &mut State) {
    if let Some(child) = &mut state.child_menu {
        settle_submenus(child);
        if let Some(done) = child.done {
            if done.is_some() {
                state.done = Some(done);
            }
            close_submenu(state);
            render(state);
        }
    }
}

fn with_menu(hwnd: HWND, f: impl FnOnce(&mut State)) {
    with_state(|root| {
        if let Some(panel) = panel_mut(root, hwnd) {
            f(panel);
        }
        if let Some(action) = pending_action(root) {
            match action {
                MenuAction::Close => root.done = Some(None),
                MenuAction::KeepOpen(items) => root.update_items(items),
                MenuAction::UpdateSlider {
                    id,
                    value,
                    label,
                    checked,
                } => root.update_slider_item(id, value, &label, checked),
            }
        }
        settle_submenus(root);
    });
}

fn panel_rect(st: &State) -> RECT {
    RECT {
        left: st.pos.x,
        top: st.pos.y,
        right: st.pos.x + st.surface.width as i32,
        bottom: st.pos.y + st.surface.height as i32,
    }
}

fn submenu_position(
    parent: RECT,
    row_y: i32,
    width: i32,
    height: i32,
    work: RECT,
    prefer_left: bool,
) -> (i32, i32) {
    let x = if prefer_left && parent.left + 4 - width >= work.left {
        parent.left + 4 - width
    } else if parent.right - 4 + width <= work.right {
        parent.right - 4
    } else {
        parent.left + 4 - width
    };
    (
        x.clamp(work.left, (work.right - width).max(work.left)),
        row_y.clamp(work.top, (work.bottom - height).max(work.top)),
    )
}

fn close_submenu(st: &mut State) {
    st.child_menu = None;
    st.open.fill(false);
    for row in &mut st.rows {
        if let RowKind::Group { open, .. } = &mut row.kind {
            *open = false;
        }
    }
    MENU_KEYBOARD.store(st.hwnd.0 as isize, Ordering::SeqCst);
}

fn refresh_submenu(st: &mut State) {
    if st.child_menu.is_none() {
        return;
    }
    let Some(index) = st.open.iter().position(|open| *open) else {
        close_submenu(st);
        return;
    };
    let Some(Item::Group { items, .. }) = st.items.get(index) else {
        close_submenu(st);
        return;
    };
    let items = items.clone();
    let bounds = panel_rect(st);
    let row_y = st.rows.get(index).map_or(0.0, |r| r.y);
    if let Some(child) = &mut st.child_menu {
        child.parent_bounds = Some(bounds);
        child.prefer_left = st
            .parent_bounds
            .is_some_and(|parent| st.pos.x < parent.left);
        child.anchor.y = bounds.top + ((row_y - PAD) * st.scale) as i32;
        child.update_items(items);
    }
}

fn open_submenu(st: &mut State, index: usize, keyboard: bool) {
    if st.open.get(index) == Some(&true) {
        if let Some(child) = st.child_menu.as_mut() {
            if keyboard {
                child.hover = child.rows.iter().position(|r| r.kind.selectable());
                MENU_KEYBOARD.store(child.hwnd.0 as isize, Ordering::SeqCst);
                render(child);
            }
            return;
        }
    }
    let Some(Item::Group { items, .. }) = st.items.get(index) else {
        return;
    };
    let items = items.clone();
    close_submenu(st);
    let Ok(hwnd) = create_window(Some(st.hwnd), true) else {
        return;
    };
    let mut texts = Vec::new();
    let entries = build(&st.dwrite, &st.font, &st.label_font, &items, &mut texts);
    let width = text_width(&entries, &texts, st.scale, false).clamp(MIN_W, MAX_W);
    let bounds = panel_rect(st);
    let row_y = st.rows.get(index).map_or(0.0, |r| r.y);
    let Ok(surface) = Surface::new(1, 1) else {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return;
    };
    let mut child = Box::new(State {
        hwnd,
        child_menu: None,
        parent_bounds: Some(bounds),
        prefer_left: st
            .parent_bounds
            .is_some_and(|parent| st.pos.x < parent.left),
        pending_action: None,
        items,
        scroll: 0.0,
        content_height: 0.0,
        keyboard_pointer: st.keyboard_pointer,
        open: vec![false; entries.len()],
        entries,
        texts,
        rows: Vec::new(),
        hover: None,
        tracking: false,
        dragging_slider: None,
        pending_slider: None,
        scale: st.scale,
        width,
        anchor: POINT {
            x: bounds.right,
            y: bounds.top + ((row_y - PAD) * st.scale) as i32,
        },
        up: None,
        work: st.work,
        surface,
        pos: POINT::default(),
        target: st.target.clone(),
        done: None,
        dwrite: st.dwrite.clone(),
        font: st.font.clone(),
        label_font: st.label_font.clone(),
    });
    relayout(&mut child);
    if keyboard {
        child.hover = child.rows.iter().position(|r| r.kind.selectable());
    }
    PANELS.lock().unwrap().push(hwnd.0 as isize);
    MENU_KEYBOARD.store(hwnd.0 as isize, Ordering::SeqCst);
    render(&mut child);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    st.open[index] = true;
    st.child_menu = Some(child);
    relayout(st);
    render(st);
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state(f: impl FnOnce(&mut State)) {
    let ran = STATE.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            if let Some(state) = guard.as_mut() {
                f(state);
                return true;
            }
        }
        false
    });
    if ran {
        crate::ui::flush();
    }
}

/// Actualiza solo un grupo; conserva el resto de ajustes y los grupos abiertos.
pub fn replace_group(label: &str, items: Vec<Item>) {
    with_state(|state| {
        if let Some((
            index,
            Item::Group {
                items: children, ..
            },
        )) = state
            .items
            .iter_mut()
            .enumerate()
            .find(|(_, item)| matches!(item, Item::Group { label: name, .. } if name == label))
        {
            if *children == items {
                return;
            }
            *children = items;
            if state.open.get(index) == Some(&true) {
                refresh_submenu(state);
            }
        }
    });
}

fn run(items: Vec<Item>, from_tray: bool, listen_to_keyboard: bool) -> Result<Option<usize>> {
    unsafe {
        let mut anchor = POINT::default();
        GetCursorPos(&mut anchor)?;
        // La barra se oculta al perder el foco. El menú de bandeja recibe mouse/teclado
        // sin activarse: Explorer conserva el foco sin cambiar la preferencia de autoocultar.
        let focus = if from_tray {
            let Some(taskbar) = tray_host_at(anchor).filter(|taskbar| focus_taskbar(*taskbar))
            else {
                // Sin Explorer en primer plano, activar el menú ocultaría la barra.
                return Ok(None);
            };
            Some(taskbar)
        } else {
            None
        };
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
        let entries = build(&dwrite, &font, &label_font, &items, &mut texts);
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

        // Un dueño oculto propio mantiene el menú vivo aunque la isla se oculte,
        // y evita prestar ventanas de Explorer que pertenecen a otro hilo/proceso.
        struct Host(HWND);
        impl Drop for Host {
            fn drop(&mut self) {
                unsafe {
                    let _ = DestroyWindow(self.0);
                }
            }
        }
        let host = Host(CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("STATIC"),
            w!("NotchIslaMenuHost"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?);
        let hwnd = create_window(Some(host.0), from_tray)?;
        let open = vec![false; entries.len()];
        STATE.with(|cell| {
            *cell.borrow_mut() = Some(State {
                hwnd,
                child_menu: None,
                parent_bounds: None,
                prefer_left: false,
                pending_action: None,
                items,
                scroll: 0.0,
                content_height: 0.0,
                keyboard_pointer: None,
                entries,
                texts,
                open,
                rows: Vec::new(),
                hover: None,
                tracking: false,
                dragging_slider: None,
                pending_slider: None,
                scale,
                width,
                anchor,
                up: None,
                work: info.rcWork,
                // Se reemplaza en `relayout` con el tamaño real.
                surface: Surface::new(1, 1).expect("superficie mínima"),
                pos: POINT::default(),
                target,
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
        MENU_KEYBOARD.store(hwnd.0 as isize, Ordering::SeqCst);
        PANELS.lock().unwrap().push(hwnd.0 as isize);
        let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0).ok();
        MENU_MOUSE_HOOK.store(hook.is_some(), Ordering::SeqCst);
        let keyboard = focus
            .filter(|_| listen_to_keyboard)
            .and_then(|_| SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), None, 0).ok());
        if let Some(focus) = focus {
            // El modo pasivo depende del origen, no del éxito del gancho de teclado.
            MENU_FOCUS.store(focus.0 as isize, Ordering::SeqCst);
            SetTimer(Some(hwnd), FOCUS_TIMER, 100, None);
        }
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        if !from_tray {
            let _ = SetForegroundWindow(hwnd);
        }

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
            crate::ui::promote_timers();
        }

        if let Some(hook) = hook {
            let _ = UnhookWindowsHookEx(hook);
        }
        if let Some(keyboard) = keyboard {
            let _ = UnhookWindowsHookEx(keyboard);
        }
        MENU_FOCUS.store(0, Ordering::SeqCst);
        MENU_MOUSE_HOOK.store(false, Ordering::SeqCst);
        SLIDER_HWND.store(0, Ordering::SeqCst);
        MENU_HWND.store(0, Ordering::SeqCst);
        MENU_KEYBOARD.store(0, Ordering::SeqCst);
        PANELS.lock().unwrap().clear();
        let state = STATE.with(|c| c.borrow_mut().take());
        let _ = DestroyWindow(hwnd);
        Ok(state.and_then(|s| s.done).flatten())
    }
}

fn taskbar_at(anchor: POINT) -> Option<HWND> {
    unsafe {
        let monitor = MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST);
        let primary = FindWindowW(w!("Shell_TrayWnd"), None).ok()?;
        if MonitorFromWindow(primary, MONITOR_DEFAULTTONEAREST) == monitor {
            return Some(primary);
        }
        let mut previous = None;
        while let Ok(window) = FindWindowExW(None, previous, w!("Shell_SecondaryTrayWnd"), None) {
            if MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) == monitor {
                return Some(window);
            }
            previous = Some(window);
        }
        Some(primary)
    }
}

fn tray_host_at(anchor: POINT) -> Option<HWND> {
    unsafe {
        // El desplegable de iconos es una ventana independiente de la barra.
        // Pasar el foco a Shell_TrayWnd lo cierra, aunque siga siendo Explorer.
        // También conserva ese host al pasar de configuración al panel de batería.
        for candidate in [GetForegroundWindow(), WindowFromPoint(anchor)] {
            let root = GetAncestor(candidate, GA_ROOT);
            let mut class = [0u16; 128];
            let len = GetClassNameW(root, &mut class) as usize;
            let class = String::from_utf16_lossy(&class[..len]);
            if matches!(
                class.as_str(),
                "NotifyIconOverflowWindow" | "TopLevelWindowForOverflowXamlIsland"
            ) && IsWindowVisible(root).as_bool()
                && MonitorFromWindow(root, MONITOR_DEFAULTTONEAREST)
                    == MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST)
            {
                return Some(root);
            }
        }
    }
    taskbar_at(anchor)
}

fn foreground_is(window: HWND) -> bool {
    unsafe {
        let foreground = GetForegroundWindow();
        foreground == window || GetAncestor(foreground, GA_ROOTOWNER) == window
    }
}

fn focus_taskbar(window: HWND) -> bool {
    unsafe {
        if foreground_is(window) {
            return true;
        }
        let _ = SetForegroundWindow(window);
        if foreground_is(window) {
            return true;
        }
        // Al pasar del menú anterior a otro panel, Explorer puede tener su cola
        // de entrada en otro hilo. Compartirla solo durante la transferencia.
        let current = GetCurrentThreadId();
        let foreground = GetWindowThreadProcessId(GetForegroundWindow(), None);
        if foreground == 0 || foreground == current {
            return false;
        }
        if !AttachThreadInput(current, foreground, true).as_bool() {
            return false;
        }
        let _ = SetForegroundWindow(window);
        let _ = AttachThreadInput(current, foreground, false);
        foreground_is(window)
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let focus = HWND(MENU_FOCUS.load(Ordering::SeqCst) as _);
    let hwnd = HWND(MENU_KEYBOARD.load(Ordering::SeqCst) as _);
    if code >= 0 && !focus.is_invalid() && !hwnd.is_invalid() {
        let event = wparam.0 as u32;
        let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) }.vkCode as u16;
        let navigation = [
            VK_UP.0,
            VK_DOWN.0,
            VK_LEFT.0,
            VK_RIGHT.0,
            VK_RETURN.0,
            VK_SPACE.0,
            VK_ESCAPE.0,
        ]
        .contains(&key);
        if foreground_is(focus) && navigation {
            if matches!(event, WM_KEYDOWN | WM_SYSKEYDOWN) {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_KEYDOWN, WPARAM(key as usize), LPARAM(0));
                }
            }
            if matches!(event, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP) {
                return LRESULT(1);
            }
        } else if matches!(event, WM_KEYDOWN | WM_SYSKEYDOWN) {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(MENU_HWND.load(Ordering::SeqCst) as _)),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn create_window(owner: Option<HWND>, passive: bool) -> Result<HWND> {
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
            WS_EX_LAYERED
                | WS_EX_TOPMOST
                | WS_EX_TOOLWINDOW
                | if passive {
                    WS_EX_NOACTIVATE
                } else {
                    Default::default()
                },
            class,
            w!("Menú de Notch Isla"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            owner,
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
    items: &[Item],
    texts: &mut Vec<Text>,
) -> Vec<Entry> {
    fn add(
        dwrite: &IDWriteFactory,
        format: &IDWriteTextFormat,
        text: &str,
        texts: &mut Vec<Text>,
    ) -> usize {
        if let Some(t) = layout(dwrite, format, text).or_else(|| layout(dwrite, format, " ")) {
            texts.push(t);
        }
        texts.len().saturating_sub(1)
    }
    items
        .iter()
        .map(|item| match item {
            Item::Slider {
                id,
                label,
                value,
                min,
                max,
            } => Entry::Slider {
                id: *id,
                text: add(dwrite, font, label, texts),
                value: *value,
                min: *min,
                max: *max,
            },
            Item::Action { id, label, checked } => Entry::Action {
                id: *id,
                text: add(dwrite, font, label, texts),
                checked: *checked,
            },
            Item::Label(label) => Entry::Label {
                text: add(dwrite, label_font, label, texts),
            },
            Item::Separator => Entry::Separator,
            Item::Group { label, .. } => Entry::Group {
                text: add(dwrite, font, label, texts),
            },
            Item::DeviceStatus {
                name,
                status,
                icon,
                connection,
            } => Entry::DeviceStatus {
                text: add(dwrite, font, name, texts),
                status: add(dwrite, label_font, status, texts),
                icon: *icon,
                connection: *connection,
            },
        })
        .collect()
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
            Entry::Action { text, .. } | Entry::Label { text } | Entry::Slider { text, .. } => {
                texts[*text].w / scale + indent + 2.0 * (PAD + TEXT_X) + RIGHT
            }
            Entry::Separator => 0.0,
            Entry::Group { text, .. } => texts[*text].w / scale + 2.0 * (PAD + TEXT_X) + RIGHT,
            Entry::DeviceStatus { text, status, .. } => {
                (texts[*text].w + texts[*status].w) / scale
                    + 2.0 * PAD
                    + TEXT_X
                    + RIGHT
                    + DEVICE_ICON_SPACE
                    + DEVICE_STATUS_GAP
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
            RowKind::Slider { .. } => SLIDER_H,
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
            Entry::Slider {
                id,
                text,
                value,
                min,
                max,
            } => push(
                RowKind::Slider {
                    id: *id,
                    value: *value,
                    min: *min,
                    max: *max,
                },
                Some(*text),
                false,
                &mut rows,
            ),
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
            Entry::Group { text, .. } => push(
                RowKind::Group {
                    index,
                    open: st.open[index],
                },
                Some(*text),
                false,
                &mut rows,
            ),
            Entry::DeviceStatus {
                text,
                status,
                icon,
                connection,
            } => push(
                RowKind::DeviceStatus {
                    status: *status,
                    icon: *icon,
                    connection: *connection,
                },
                Some(*text),
                false,
                &mut rows,
            ),
        }
    }
    let height = y + PAD;
    st.content_height = height;
    let viewport = height.min((st.work.bottom - st.work.top) as f32 / st.scale);
    st.scroll = st.scroll.clamp(0.0, (height - viewport).max(0.0));
    for row in &mut rows {
        row.y -= st.scroll;
    }
    st.rows = rows;

    let s = st.scale;
    let w = (st.width * s).ceil() as i32;
    let h = (viewport * s).floor() as i32;
    let work = st.work;
    // Hacia la derecha del cursor si cabe; si no, hacia la izquierda.
    let (x, y) = if let Some(parent) = st.parent_bounds {
        submenu_position(parent, st.anchor.y, w, h, work, st.prefer_left)
    } else {
        let x = if st.anchor.x + w <= work.right {
            st.anchor.x
        } else {
            st.anchor.x - w
        }
        .max(work.left);
        let up = *st.up.get_or_insert(st.anchor.y + h > work.bottom);
        let y = if up {
            (st.anchor.y - h).max(work.top)
        } else {
            st.anchor.y.min(work.bottom - h).max(work.top)
        };
        (x, y)
    };
    st.pos = POINT { x, y };
    if st.surface.width != w as u32 || st.surface.height != h as u32 {
        if let Ok(surface) = Surface::new(w.max(1) as u32, h.max(1) as u32) {
            st.surface = surface;
        }
    }
}

fn draw_menu_text(
    target: &ID2D1DCRenderTarget,
    text: &Text,
    x: f32,
    y: f32,
    clip: D2D_RECT_F,
    color: D2D1_COLOR_F,
) {
    if clip.right <= clip.left || clip.bottom <= clip.top {
        return;
    }
    unsafe {
        if let Ok(brush) = target.CreateSolidColorBrush(&color, None) {
            target.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
            target.DrawTextLayout(
                Vector2 { X: x, Y: y },
                &text.layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
            target.PopAxisAlignedClip();
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
                if (st.hover == Some(i) && kind.selectable())
                    || matches!(kind, RowKind::Group { open: true, .. })
                {
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
                if let RowKind::DeviceStatus {
                    icon, connection, ..
                } = kind
                {
                    paint.set_color_rgba8(235, 235, 240, 235);
                    crate::island::draw_notice_icon(
                        pixmap,
                        &paint,
                        icon,
                        (PAD + TEXT_X + 9.0) * s,
                        cy,
                        0.8 * s,
                    );
                    paint.set_color_rgba8(173, 193, 220, 240);
                    crate::island::draw_notice_icon(pixmap, &paint, connection, cx, cy, 0.72 * s);
                }
                if let RowKind::Slider {
                    value, min, max, ..
                } = kind
                {
                    let (left, right) = slider_track(st.width, row.child);
                    let left = left * s;
                    let right = right * s;
                    let cy = y0 + rh - 14.0 * s;
                    let fraction =
                        value.saturating_sub(min) as f32 / max.saturating_sub(min).max(1) as f32;
                    let thumb = left + (right - left) * fraction.clamp(0.0, 1.0);
                    paint.set_color_rgba8(255, 255, 255, 50);
                    fill_rounded_rect(
                        pixmap,
                        &paint,
                        left,
                        cy - 2.0 * s,
                        right - left,
                        4.0 * s,
                        2.0 * s,
                    );
                    paint.set_color_rgba8(10, 132, 255, 255);
                    fill_rounded_rect(
                        pixmap,
                        &paint,
                        left,
                        cy - 2.0 * s,
                        thumb - left,
                        4.0 * s,
                        2.0 * s,
                    );
                    paint.set_color_rgba8(255, 255, 255, 255);
                    fill_rounded_rect(
                        pixmap,
                        &paint,
                        thumb - 5.0 * s,
                        cy - 5.0 * s,
                        10.0 * s,
                        10.0 * s,
                        5.0 * s,
                    );
                }
                let mut pb = PathBuilder::new();
                match kind {
                    RowKind::Action { checked: true, .. } => {
                        paint.set_color_rgba8(255, 255, 255, 255);
                        pb.move_to(cx - 4.5 * s, cy);
                        pb.line_to(cx - 1.5 * s, cy + 3.0 * s);
                        pb.line_to(cx + 4.5 * s, cy - 3.5 * s);
                    }
                    RowKind::Group { .. } => {
                        pb.move_to(cx - 2.0 * s, cy - 4.0 * s);
                        pb.line_to(cx + 2.0 * s, cy);
                        pb.line_to(cx - 2.0 * s, cy + 4.0 * s);
                    }
                    _ => {}
                }
                stroke(pixmap, &paint, pb, 1.6 * s);
            }
        }
    }
    let viewport = h / s;
    if st.content_height > viewport {
        let track = (viewport - 2.0 * RADIUS).max(1.0);
        let thumb = (track * viewport / st.content_height).max(18.0).min(track);
        let y = RADIUS + (track - thumb) * st.scroll / (st.content_height - viewport);
        paint.set_color_rgba8(255, 255, 255, 100);
        fill_rounded_rect(pixmap, &paint, w - 4.0 * s, y * s, 2.0 * s, thumb * s, s);
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
                let status = match row.kind {
                    RowKind::DeviceStatus { status, .. } => st.texts.get(status),
                    _ => None,
                };
                let indent = if row.child { CHILD_INDENT } else { 0.0 }
                    + if status.is_some() {
                        DEVICE_ICON_SPACE
                    } else {
                        0.0
                    };
                let x = (PAD + TEXT_X + indent) * s;
                let y = if matches!(row.kind, RowKind::Slider { .. }) {
                    row.y * s + 6.0 * s
                } else {
                    row.y * s + (row.h * s - text.h) / 2.0
                };
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
                    top: (row.y * s).max(PAD * s),
                    right: w - (PAD + RIGHT) * s,
                    bottom: ((row.y + row.h) * s).min(h - PAD * s),
                };
                if clip.bottom <= clip.top {
                    continue;
                }
                let mut name_clip = clip;
                if let Some(status) = status {
                    let status_x = (clip.right - status.w).max(x);
                    name_clip.right = status_x - DEVICE_STATUS_GAP * s;
                    let status_clip = D2D_RECT_F {
                        left: status_x,
                        ..clip
                    };
                    let status_y = row.y * s + (row.h * s - status.h) / 2.0;
                    let color = D2D1_COLOR_F {
                        r: 0.78,
                        g: 0.81,
                        b: 0.85,
                        a: 1.0,
                    };
                    draw_menu_text(&st.target, status, status_x, status_y, status_clip, color);
                }
                draw_menu_text(&st.target, text, x, y, name_clip, color);
            }
            let _ = st.target.EndDraw(None, None);
        }

        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
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

fn slider_track(width: f32, child: bool) -> (f32, f32) {
    (
        PAD + TEXT_X + if child { CHILD_INDENT } else { 0.0 },
        width - PAD - TEXT_X,
    )
}

fn slider_value(x: f32, left: f32, right: f32, min: u32, max: u32) -> u32 {
    let fraction = ((x - left) / (right - left).max(1.0)).clamp(0.0, 1.0);
    min + (fraction * max.saturating_sub(min) as f32).round() as u32
}

fn update_slider(st: &mut State, x: f32) {
    let Some(id) = st.dragging_slider else { return };
    let Some(row) = st
        .rows
        .iter()
        .find(|row| matches!(row.kind, RowKind::Slider { id: slider_id, .. } if slider_id == id))
    else {
        return;
    };
    if let RowKind::Slider {
        value, min, max, ..
    } = row.kind
    {
        let (left, right) = slider_track(st.width, row.child);
        let next = slider_value(x / st.scale, left, right, min, max);
        if next != value {
            dispatch(st, id + next as usize);
        }
    }
}

fn dispatch(st: &mut State, id: usize) {
    let action = HANDLER.with(|cell| {
        if let Some((ptr, invoke)) = cell.get() {
            unsafe { invoke(ptr, id) }
        } else {
            MenuAction::Close
        }
    });
    st.pending_action = Some(action);
}

/// Elige la fila `i`: una opción termina el menú o lo mantiene abierto según `MenuAction`; un grupo se despliega o se pliega.
fn activate(st: &mut State, i: usize) {
    match st.rows.get(i).map(|r| r.kind) {
        Some(RowKind::Action { id, .. }) => {
            dispatch(st, id);
        }
        Some(RowKind::Group { index, .. }) => {
            open_submenu(st, index, true);
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
            let row = &st.rows[i as usize];
            let viewport = st.surface.height as f32 / st.scale;
            let delta = if row.y < PAD {
                row.y - PAD
            } else {
                (row.y + row.h - viewport + PAD).max(0.0)
            };
            if delta != 0.0 {
                st.scroll += delta;
                relayout(st);
            }
            render(st);
            return;
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_MOUSEMOVE && SLIDER_HWND.load(Ordering::SeqCst) != 0 {
        // Durante el arrastre pasivo el gancho ya envía el movimiento al panel dueño.
        return LRESULT(0);
    }
    let deferrable = matches!(
        msg,
        WM_MOUSEMOVE
            | WM_SLIDER_MOVE
            | WM_MOUSEWHEEL
            | 0x02A3
            | WM_LBUTTONDOWN
            | WM_LBUTTONUP
            | WM_RBUTTONUP
            | WM_KEYDOWN
            | WM_CAPTURECHANGED
            | WM_ACTIVATE
            | WM_CLOSE
            | WM_TIMER
    );
    if deferrable && STATE.with(|state| state.try_borrow_mut().is_err()) {
        crate::ui::defer(wndproc, hwnd, msg, wparam, lparam);
        return LRESULT(0);
    }
    match msg {
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) as u16 as i16) as f32 / 120.0;
            with_menu(hwnd, |st| {
                if st.dragging_slider.is_none() && SLIDER_HWND.load(Ordering::SeqCst) == 0 {
                    close_submenu(st);
                    st.scroll -= delta * ROW_H * 3.0;
                    st.hover = None;
                    st.keyboard_pointer = None;
                    relayout(st);
                    render(st);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEACTIVATE
            if MENU_FOCUS.load(Ordering::SeqCst) != 0
                || hwnd.0 as isize != MENU_HWND.load(Ordering::SeqCst) =>
        {
            LRESULT(MA_NOACTIVATE as isize)
        }
        WM_TIMER if wparam.0 == FOCUS_TIMER => {
            let focus = HWND(MENU_FOCUS.load(Ordering::SeqCst) as _);
            if !focus.is_invalid() && !foreground_is(focus) {
                with_menu(hwnd, |st| st.done = Some(None));
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == SUBMENU_TIMER => {
            unsafe {
                let _ = KillTimer(Some(hwnd), SUBMENU_TIMER);
            }
            if SLIDER_HWND.load(Ordering::SeqCst) != 0 {
                return LRESULT(0);
            }
            with_menu(hwnd, |st| {
                if let Some(RowKind::Group { index, .. }) =
                    st.hover.and_then(|i| st.rows.get(i)).map(|r| r.kind)
                {
                    open_submenu(st, index, false);
                } else if st.hover.is_some() {
                    close_submenu(st);
                    render(st);
                }
            });
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == SLIDER_TIMER => {
            unsafe {
                let _ = KillTimer(Some(hwnd), SLIDER_TIMER);
            }
            with_menu(hwnd, |st| {
                if let Some(x) = st.pending_slider.take() {
                    update_slider(st, x);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE | WM_SLIDER_MOVE => {
            let x = (lparam.0 & 0xFFFF) as u16 as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
            with_menu(hwnd, |st| {
                if st.dragging_slider.is_some() {
                    // La vista previa consume el movimiento más reciente por cuadro,
                    // incluso si el mouse envía cientos de eventos por segundo.
                    if st.pending_slider.replace(x).is_none() {
                        unsafe {
                            SetTimer(Some(hwnd), SLIDER_TIMER, 16, None);
                        }
                    }
                    return;
                }
                if msg == WM_SLIDER_MOVE {
                    return;
                }
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
                let pointer = POINT {
                    x: st.pos.x + x as i32,
                    y: st.pos.y + y as i32,
                };
                if st
                    .keyboard_pointer
                    .is_some_and(|old| old.x == pointer.x && old.y == pointer.y)
                {
                    return;
                }
                st.keyboard_pointer = None;
                let hover = row_at(st, y);
                if hover != st.hover {
                    st.hover = hover;
                    unsafe {
                        SetTimer(Some(hwnd), SUBMENU_TIMER, 180, None);
                    }
                    MENU_KEYBOARD.store(hwnd.0 as isize, Ordering::SeqCst);
                    render(st);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as u16 as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
            with_menu(hwnd, |st| {
                if let Some(RowKind::Slider { id, .. }) = row_at(st, y).map(|i| st.rows[i].kind) {
                    st.dragging_slider = Some(id);
                    if MENU_MOUSE_HOOK.load(Ordering::SeqCst)
                        && MENU_FOCUS.load(Ordering::SeqCst) != 0
                    {
                        SLIDER_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
                    } else {
                        unsafe {
                            SetCapture(hwnd);
                        }
                    }
                    update_slider(st, x);
                }
            });
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            unsafe {
                let _ = KillTimer(Some(hwnd), SLIDER_TIMER);
            }
            with_menu(hwnd, |st| {
                if let Some(x) = st.pending_slider.take() {
                    update_slider(st, x);
                }
                st.dragging_slider = None;
            });
            LRESULT(0)
        }
        // WM_MOUSELEAVE
        0x02A3 => {
            with_menu(hwnd, |st| {
                st.tracking = false;
                if st.hover.is_some() && st.keyboard_pointer.is_none() {
                    st.hover = None;
                    render(st);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as u16 as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
            with_menu(hwnd, |st| {
                if st.dragging_slider.is_some() {
                    st.pending_slider = None;
                    unsafe {
                        let _ = KillTimer(Some(hwnd), SLIDER_TIMER);
                    }
                    update_slider(st, x);
                    st.dragging_slider = None;
                    let _ = SLIDER_HWND.compare_exchange(
                        hwnd.0 as isize,
                        0,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                    unsafe {
                        if GetCapture() == hwnd {
                            let _ = ReleaseCapture();
                        }
                    }
                    return;
                }
                if let Some(i) = row_at(st, y) {
                    activate(st, i);
                }
            });
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let target = MENU_KEYBOARD.load(Ordering::SeqCst);
            if hwnd.0 as isize == MENU_HWND.load(Ordering::SeqCst)
                && target != 0
                && target != hwnd.0 as isize
            {
                unsafe {
                    let _ = PostMessageW(Some(HWND(target as _)), WM_KEYDOWN, wparam, lparam);
                }
                return LRESULT(0);
            }
            let key = wparam.0 as u16;
            with_menu(hwnd, |st| {
                let mut pointer = POINT::default();
                if unsafe { GetCursorPos(&mut pointer) }.is_ok() {
                    st.keyboard_pointer = Some(pointer);
                }
                match key {
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
                        if let Some(RowKind::Slider {
                            id,
                            value,
                            min,
                            max,
                        }) = st.hover.and_then(|i| st.rows.get(i)).map(|r| r.kind)
                        {
                            let next = if k == VK_RIGHT.0 {
                                value.saturating_add(1).min(max)
                            } else {
                                value.saturating_sub(1).max(min)
                            };
                            if next != value {
                                dispatch(st, id + next as usize);
                            }
                            return;
                        }
                        if k == VK_LEFT.0 {
                            if st.parent_bounds.is_some() {
                                st.done = Some(None);
                            } else {
                                close_submenu(st);
                                render(st);
                            }
                        } else if let Some(i) = st.hover {
                            if let RowKind::Group { index, .. } = st.rows[i].kind {
                                open_submenu(st, index, true);
                            }
                        }
                    }
                    _ => {}
                }
            });
            LRESULT(0)
        }
        // Otra ventana pasó al frente (Alt+Tab, clic en otra app): se cierra.
        WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            if MENU_FOCUS.load(Ordering::SeqCst) == 0
                && hwnd.0 as isize == MENU_HWND.load(Ordering::SeqCst)
            {
                with_menu(hwnd, |st| st.done = Some(None));
            }
            LRESULT(0)
        }
        // Lo manda el gancho de mouse al hacer clic fuera.
        WM_CLOSE | WM_DESTROY => {
            with_menu(hwnd, |st| st.done = Some(None));
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Gancho de mouse de todo el sistema, solo mientras el menú está abierto: un clic fuera de
/// él lo cierra (el clic sigue su curso normal).
fn panel_at(point: POINT) -> Option<HWND> {
    let panels = PANELS.lock().unwrap().clone();
    panels.iter().rev().find_map(|id| {
        let hwnd = HWND(*id as _);
        let mut rect = RECT::default();
        (unsafe { IsWindowVisible(hwnd).as_bool() && GetWindowRect(hwnd, &mut rect).is_ok() }
            && point.x >= rect.left
            && point.x < rect.right
            && point.y >= rect.top
            && point.y < rect.bottom)
            .then_some(hwnd)
    })
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let root = MENU_HWND.load(Ordering::SeqCst);
    if code >= 0 && root != 0 {
        let event = wparam.0 as u32;
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let hwnd = HWND(SLIDER_HWND.load(Ordering::SeqCst) as _);
        if !hwnd.is_invalid() && matches!(event, WM_MOUSEMOVE | WM_LBUTTONUP) {
            let mut rect = RECT::default();
            if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
                let x = (info.pt.x - rect.left) as i16 as u16 as usize;
                let y = (info.pt.y - rect.top) as i16 as u16 as usize;
                let forwarded = if event == WM_MOUSEMOVE {
                    WM_SLIDER_MOVE
                } else {
                    event
                };
                let posted = unsafe {
                    PostMessageW(
                        Some(hwnd),
                        forwarded,
                        WPARAM(0),
                        LPARAM((x | (y << 16)) as isize),
                    )
                    .is_ok()
                };
                // Consumir el movimiento congela el cursor. Solo la suelta se reserva
                // al deslizador, para que no active un control debajo del puntero.
                if posted && event == WM_LBUTTONUP {
                    return LRESULT(1);
                }
            }
        }
        if event == WM_MOUSEWHEEL {
            if let Some(hwnd) = panel_at(info.pt) {
                unsafe {
                    let _ = PostMessageW(
                        Some(hwnd),
                        event,
                        WPARAM(info.mouseData as usize),
                        LPARAM(0),
                    );
                }
                return LRESULT(1);
            }
        }
        if matches!(event, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN)
            && panel_at(info.pt).is_none()
        {
            unsafe {
                let _ = PostMessageW(Some(HWND(root as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submenus_se_colocan_al_lado_y_caben_en_el_monitor() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1000,
            bottom: 800,
        };
        let parent = RECT {
            left: 100,
            top: 100,
            right: 300,
            bottom: 500,
        };
        assert_eq!(
            submenu_position(parent, 200, 250, 200, work, false),
            (296, 200)
        );
        let parent = RECT {
            left: 700,
            top: 100,
            right: 900,
            bottom: 500,
        };
        assert_eq!(
            submenu_position(parent, 750, 250, 200, work, false),
            (454, 600)
        );
        assert_eq!(
            submenu_position(parent, 200, 250, 200, work, true),
            (454, 200)
        );
        let work = RECT {
            left: -1920,
            top: -200,
            right: 0,
            bottom: 880,
        };
        for y in [-1000, -200, 0, 850, 1500] {
            let (x, top) = submenu_position(
                RECT {
                    left: -300,
                    top: 0,
                    right: -100,
                    bottom: 500,
                },
                y,
                300,
                700,
                work,
                true,
            );
            assert!(x >= work.left && x + 300 <= work.right);
            assert!(top >= work.top && top + 700 <= work.bottom);
        }
    }

    fn save_cascade_preview(st: &State) {
        if std::env::var_os("NOTCH_MENU_PREVIEW").is_none() {
            return;
        }
        fn write(st: &State, index: &mut usize, rows: &mut Vec<String>) {
            let filename = format!("panel-{}.rgba", *index);
            let bgra = unsafe {
                std::slice::from_raw_parts(
                    st.surface.bits,
                    (st.surface.width * st.surface.height * 4) as usize,
                )
            };
            let rgba: Vec<u8> = bgra
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0], p[3]])
                .collect();
            std::fs::write(format!("target/cascade-preview/{filename}"), rgba).unwrap();
            rows.push(format!(
                "{{\"x\":{},\"y\":{},\"w\":{},\"h\":{},\"file\":\"{filename}\"}}",
                st.pos.x, st.pos.y, st.surface.width, st.surface.height
            ));
            *index += 1;
            if let Some(child) = &st.child_menu {
                write(child, index, rows);
            }
        }
        std::fs::create_dir_all("target/cascade-preview").unwrap();
        let mut rows = Vec::new();
        write(st, &mut 0, &mut rows);
        std::fs::write(
            "target/cascade-preview/panels.json",
            format!("[{}]", rows.join(",")),
        )
        .unwrap();
    }

    #[test]
    #[ignore = "abre submenús laterales y comprueba navegación, actualización y tamaño del padre"]
    fn submenus_conservan_el_menu_principal_compacto() {
        use std::time::{Duration, Instant};
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_APP};
        const REFRESH: u32 = WM_APP + 100;
        const CHECK: u32 = WM_APP + 101;
        const SNAPSHOT: u32 = WM_APP + 102;
        const KEY: u32 = WM_APP + 103;
        fn nested() -> Item {
            Item::Group {
                label: "Receptores sin enlace confirmado".into(),
                items: vec![Item::Action {
                    id: 3,
                    label: "Acción anidada".into(),
                    checked: false,
                }],
            }
        }
        extern "system" fn host_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
            if msg == KEY {
                if !STATE.with(|s| s.try_borrow_mut().is_ok()) {
                    return LRESULT(0);
                }
                let target = HWND(MENU_KEYBOARD.load(Ordering::SeqCst) as _);
                wndproc(target, WM_KEYDOWN, wp, lp);
                return LRESULT(1);
            }
            if msg == SNAPSHOT {
                with_state(|st| save_cascade_preview(st));
                return LRESULT(0);
            }
            if msg == REFRESH || msg == CHECK {
                if msg == REFRESH {
                    let mut devices: Vec<Item> = (0..80)
                        .map(|i| Item::Label(format!("Dispositivo {i}: 78 %")))
                        .collect();
                    devices.push(nested());
                    replace_group("Dispositivos y batería", devices);
                }
                let mut bits = 0;
                with_state(|st| {
                    if st.open[0] && st.child_menu.is_some() {
                        bits |= 1;
                    }
                    if st.items.iter().any(|i| {
                        matches!(
                            i,
                            Item::Action {
                                id: 1,
                                checked: true,
                                ..
                            }
                        )
                    }) {
                        bits |= 2;
                    }
                    if st.items.iter().any(|i| {
                        matches!(
                            i,
                            Item::Slider {
                                id: 500,
                                value: 17,
                                ..
                            }
                        )
                    }) {
                        bits |= 4;
                    }
                    if st.entries.iter().any(|i| {
                        matches!(
                            i,
                            Entry::Slider {
                                id: 500,
                                value: 17,
                                ..
                            }
                        )
                    }) {
                        bits |= 8;
                    }
                    if st.content_height == 2.0 * PAD + 2.0 * ROW_H + SLIDER_H {
                        bits |= 16;
                    }
                    if let Some(child) = &st.child_menu {
                        if child.content_height > child.surface.height as f32 / child.scale {
                            bits |= 32;
                        }
                        if child.scroll > 0.0 {
                            bits |= 64;
                        }
                        if let Some(row) = child.hover.and_then(|i| child.rows.get(i)) {
                            if row.y >= 0.0
                                && row.y + row.h <= child.surface.height as f32 / child.scale
                            {
                                bits |= 128;
                            }
                        }
                        if child.child_menu.is_some() {
                            bits |= 256;
                        }
                    }
                });
                return LRESULT(bits);
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        fn items(checked: bool, radius: u32) -> Vec<Item> {
            vec![
                Item::Group {
                    label: "Dispositivos y batería".into(),
                    items: vec![
                        Item::Action {
                            id: 2,
                            label: "Actualizar".into(),
                            checked: false,
                        },
                        Item::DeviceStatus {
                            name: "MCHOSE K7 Ultra".into(),
                            status: "78 %".into(),
                            icon: crate::island::NoticeIcon::Mouse,
                            connection: crate::island::NoticeIcon::Wireless,
                        },
                        Item::DeviceStatus {
                            name: "Audífonos inalámbricos".into(),
                            status: "95 % · Cargando".into(),
                            icon: crate::island::NoticeIcon::Audio,
                            connection: crate::island::NoticeIcon::Bluetooth,
                        },
                        nested(),
                    ],
                },
                Item::Action {
                    id: 1,
                    label: "Otra opción".into(),
                    checked,
                },
                Item::Slider {
                    id: 500,
                    label: format!("Radio: {radius}"),
                    value: radius,
                    min: 0,
                    max: 40,
                },
            ]
        }
        struct Restore(HWND);
        impl Drop for Restore {
            fn drop(&mut self) {
                activate_for_test(self.0);
            }
        }
        let _restore = Restore(unsafe { GetForegroundWindow() });
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
        }
        let mut anchor = POINT::default();
        unsafe {
            GetCursorPos(&mut anchor).unwrap();
        }
        activate_for_test(taskbar_at(anchor).unwrap());
        let instance = unsafe { GetModuleHandleW(None).unwrap() };
        let class = w!("NotchIslaCascadeTest");
        unsafe {
            RegisterClassExW(&WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(host_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            });
        }
        let host = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class,
                class,
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
            .unwrap()
        };
        let host_id = host.0 as isize;
        let driver = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let root = loop {
                let hwnd = HWND(MENU_HWND.load(Ordering::SeqCst) as _);
                if !hwnd.is_invalid() && unsafe { IsWindowVisible(hwnd).as_bool() } {
                    break hwnd;
                }
                assert!(Instant::now() < deadline, "el menú debe abrirse");
                std::thread::sleep(Duration::from_millis(10));
            };
            struct Close(HWND);
            impl Drop for Close {
                fn drop(&mut self) {
                    unsafe {
                        let _ = PostMessageW(Some(self.0), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
            }
            let _close = Close(root);
            let key = |key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| {
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    if unsafe {
                        SendMessageW(HWND(host_id as _), KEY, Some(WPARAM(key.0 as usize)), None)
                    }
                    .0 == 1
                    {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "la interfaz debe procesar la tecla"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            };
            let mut before = RECT::default();
            unsafe {
                GetWindowRect(root, &mut before).unwrap();
            }
            key(VK_DOWN);
            key(VK_RIGHT);
            let child = HWND(MENU_KEYBOARD.load(Ordering::SeqCst) as _);
            assert_ne!(child, root);
            key(VK_RETURN); // Actualizar desde el hijo conserva ambas ventanas.
            assert_eq!(MENU_KEYBOARD.load(Ordering::SeqCst), child.0 as isize);
            key(VK_DOWN);
            key(VK_RIGHT);
            unsafe {
                SendMessageW(HWND(host_id as _), SNAPSHOT, None, None);
            }
            key(VK_ESCAPE);
            key(VK_ESCAPE);
            assert_eq!(MENU_KEYBOARD.load(Ordering::SeqCst), root.0 as isize);
            key(VK_DOWN);
            key(VK_RETURN); // Otra opción del padre.
            key(VK_DOWN);
            key(VK_RIGHT); // Radio 16 -> 17.
            key(VK_UP);
            key(VK_UP);
            key(VK_RIGHT);
            let child = HWND(MENU_KEYBOARD.load(Ordering::SeqCst) as _);
            let host = HWND(host_id as _);
            assert_eq!(
                unsafe { SendMessageW(host, REFRESH, None, None) }.0 & 63,
                63
            );
            let mut after = RECT::default();
            unsafe {
                GetWindowRect(root, &mut after).unwrap();
            }
            assert_eq!(
                (before.right - before.left, before.bottom - before.top),
                (after.right - after.left, after.bottom - after.top)
            );
            assert_eq!(MENU_KEYBOARD.load(Ordering::SeqCst), child.0 as isize);
            unsafe {
                SendMessageW(
                    child,
                    WM_MOUSEWHEEL,
                    Some(WPARAM(((-120i16 as u16 as u32) << 16) as usize)),
                    None,
                );
            }
            assert_ne!(unsafe { SendMessageW(host, CHECK, None, None) }.0 & 64, 0);
            key(VK_DOWN); // Salta los textos, llega al grupo y lo mantiene visible.
            assert_eq!(
                unsafe { SendMessageW(host, CHECK, None, None) }.0 & 192,
                192
            );
            key(VK_RIGHT);
            let grandchild = HWND(MENU_KEYBOARD.load(Ordering::SeqCst) as _);
            assert_ne!(grandchild, child);
            assert_ne!(unsafe { SendMessageW(host, CHECK, None, None) }.0 & 256, 0);
            let mut rect = RECT::default();
            unsafe {
                GetWindowRect(grandchild, &mut rect).unwrap();
            }
            assert_eq!(
                panel_at(POINT {
                    x: (rect.left + rect.right) / 2,
                    y: (rect.top + rect.bottom) / 2
                }),
                Some(grandchild)
            );
            key(VK_RETURN);
            assert_eq!(MENU_HWND.load(Ordering::SeqCst), root.0 as isize);
        });
        let mut checked = false;
        let mut radius = 16;
        let mut refreshes = 0;
        let mut nested_actions = 0;
        show_tray(items(checked, radius), |id| {
            match id {
                1 => checked = !checked,
                2 => refreshes += 1,
                3 => nested_actions += 1,
                value => {
                    radius = (value - 500) as u32;
                    return MenuAction::UpdateSlider {
                        id: 500,
                        value: radius,
                        label: format!("Radio: {radius}"),
                        checked: Some((1, checked)),
                    };
                }
            }
            MenuAction::KeepOpen(items(checked, radius))
        });
        let result = driver.join();
        unsafe {
            let _ = DestroyWindow(host);
        }
        result.unwrap();
        assert!(checked);
        assert_eq!((refreshes, radius, nested_actions), (1, 17, 1));
    }

    fn activate_for_test(window: HWND) {
        if foreground_is(window) {
            return;
        }
        unsafe {
            let current = GetCurrentThreadId();
            let foreground = GetWindowThreadProcessId(GetForegroundWindow(), None);
            // El test no nace de un clic real en la bandeja; prepara ese mismo foco.
            let attached =
                foreground != current && AttachThreadInput(current, foreground, true).as_bool();
            let _ = SetForegroundWindow(window);
            if attached {
                let _ = AttachThreadInput(current, foreground, false);
            }
            if !foreground_is(window) {
                use windows::Win32::UI::Input::KeyboardAndMouse::{
                    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_MENU,
                };
                let keys = [Default::default(), KEYEVENTF_KEYUP].map(|flags| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VK_MENU,
                            dwFlags: flags,
                            ..Default::default()
                        },
                    },
                });
                SendInput(&keys, std::mem::size_of::<INPUT>() as i32);
                let _ = SetForegroundWindow(window);
            }
        }
    }

    #[test]
    #[ignore = "abre ventanas de prueba que simulan los desplegables de iconos de Windows"]
    fn bandeja_conserva_el_desplegable_de_iconos_ocultos() {
        use std::time::{Duration, Instant};
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
        extern "system" fn overflow_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
            if msg == WM_ACTIVATE && (wp.0 & 0xffff) as u32 == WA_INACTIVE {
                // Igual que el desplegable: pierde foco y desaparece.
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        struct Restore(HWND);
        impl Drop for Restore {
            fn drop(&mut self) {
                activate_for_test(self.0);
            }
        }
        let _restore = Restore(unsafe { GetForegroundWindow() });
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
        }
        for class in [
            w!("NotifyIconOverflowWindow"),
            w!("TopLevelWindowForOverflowXamlIsland"),
        ] {
            let mut anchor = POINT::default();
            let instance = unsafe { GetModuleHandleW(None).unwrap() };
            unsafe {
                GetCursorPos(&mut anchor).unwrap();
                RegisterClassExW(&WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(overflow_proc),
                    hInstance: instance.into(),
                    lpszClassName: class,
                    ..Default::default()
                });
            }
            let overflow = unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    class,
                    w!("Prueba de iconos ocultos"),
                    WS_POPUP,
                    anchor.x - 100,
                    anchor.y - 100,
                    100,
                    100,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                )
                .unwrap()
            };
            struct Window(HWND);
            impl Drop for Window {
                fn drop(&mut self) {
                    unsafe {
                        let _ = DestroyWindow(self.0);
                    }
                }
            }
            let _window = Window(overflow);
            unsafe {
                let _ = ShowWindow(overflow, SW_SHOWNOACTIVATE);
            }
            activate_for_test(overflow);
            // La preparación del foco puede emitir WA_INACTIVE antes de terminar;
            // presentar el desplegable después, como hace Explorer al abrirlo.
            unsafe {
                let _ = ShowWindow(overflow, SW_SHOWNOACTIVATE);
            }
            assert!(
                foreground_is(overflow),
                "preparar foco del desplegable de prueba"
            );
            assert_eq!(tray_host_at(anchor), Some(overflow));
            let expected = overflow.0 as isize;
            let driver = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(3);
                let menu = loop {
                    let menu = HWND(MENU_HWND.load(Ordering::SeqCst) as _);
                    if !menu.is_invalid() && unsafe { IsWindowVisible(menu).as_bool() } {
                        break menu;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "el menú debe abrirse desde el desplegable"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                };
                struct Close(HWND);
                impl Drop for Close {
                    fn drop(&mut self) {
                        unsafe {
                            let _ = PostMessageW(Some(self.0), WM_CLOSE, WPARAM(0), LPARAM(0));
                        }
                    }
                }
                let _close = Close(menu);
                assert_eq!(MENU_FOCUS.load(Ordering::SeqCst), expected);
                let overflow = HWND(expected as _);
                std::thread::sleep(Duration::from_millis(150));
                assert!(foreground_is(overflow));
                assert!(
                    unsafe { IsWindowVisible(overflow).as_bool() },
                    "no debe ocultarse el desplegable"
                );
            });
            run(
                vec![Item::Label("Menú desde iconos ocultos".into())],
                true,
                true,
            )
            .unwrap();
            driver.join().unwrap();
        }
    }

    #[test]
    #[ignore = "requiere Explorer y cambia brevemente el foco al menú de bandeja"]
    fn bandeja_no_toma_el_foco_si_falla_el_gancho_de_teclado() {
        use std::time::{Duration, Instant};
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, IsWindowVisible, SendMessageW, GWL_EXSTYLE,
        };
        struct Restore(HWND);
        impl Drop for Restore {
            fn drop(&mut self) {
                focus_taskbar(self.0);
            }
        }
        let _restore = Restore(unsafe { GetForegroundWindow() });
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
        }
        unsafe {
            let mut anchor = POINT::default();
            GetCursorPos(&mut anchor).unwrap();
            activate_for_test(taskbar_at(anchor).expect("barra de tareas"));
        }
        let driver = std::thread::spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(3);
            let hwnd = loop {
                let hwnd = HWND(MENU_HWND.load(Ordering::SeqCst) as _);
                if !hwnd.is_invalid() && unsafe { IsWindowVisible(hwnd).as_bool() } {
                    break hwnd;
                }
                assert!(Instant::now() < deadline, "el menú debe abrirse");
                std::thread::sleep(Duration::from_millis(10));
            };
            struct Close(HWND);
            impl Drop for Close {
                fn drop(&mut self) {
                    unsafe {
                        let _ = PostMessageW(Some(self.0), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
            }
            let _close = Close(hwnd);
            let shell = HWND(MENU_FOCUS.load(Ordering::SeqCst) as _);
            assert!(
                !shell.is_invalid(),
                "sin gancho de teclado se sigue vigilando Explorer"
            );
            assert!(foreground_is(shell), "el menú no debe tomar el foco");
            let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
            assert_ne!(style & WS_EX_NOACTIVATE.0, 0);
            assert_eq!(
                unsafe { SendMessageW(hwnd, WM_MOUSEACTIVATE, None, None) }.0,
                MA_NOACTIVATE as isize
            );
            std::thread::sleep(Duration::from_millis(200));
            assert!(
                foreground_is(shell),
                "Explorer conserva el foco después de mostrar el menú"
            );
            assert_eq!(MENU_HWND.load(Ordering::SeqCst), hwnd.0 as isize);
        });
        run(
            vec![Item::Label("Prueba de foco sin gancho de teclado".into())],
            true,
            false,
        )
        .unwrap();
        driver.join().unwrap();
    }

    #[test]
    fn deslizador_respeta_limites_y_coordenadas_fuera_de_la_barra() {
        assert_eq!(slider_value(-100.0, 30.0, 230.0, 0, 40), 0);
        assert_eq!(slider_value(130.0, 30.0, 230.0, 0, 40), 20);
        assert_eq!(slider_value(500.0, 30.0, 230.0, 0, 40), 40);
        // La misma posición física debe elegir el mismo valor a cualquier escala DPI.
        for scale in [1.0, 1.25, 1.5, 2.0] {
            assert_eq!(
                slider_value((130.0 * scale) / scale, 30.0, 230.0, 0, 40),
                20
            );
        }
    }

    #[test]
    #[ignore = "abre un menú de prueba para verificar clics y arrastre en la misma ventana"]
    fn cambiar_opciones_y_arrastrar_no_recrea_el_menu() {
        use std::time::{Duration, Instant};
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::HiDpi::GetDpiForWindow;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            IsWindowVisible, SendMessageW, SetCursorPos,
        };
        struct Restore(HWND, POINT);
        impl Drop for Restore {
            fn drop(&mut self) {
                activate_for_test(self.0);
                unsafe {
                    let _ = SetCursorPos(self.1.x, self.1.y);
                }
            }
        }
        let mut original_pointer = POINT::default();
        unsafe {
            GetCursorPos(&mut original_pointer).unwrap();
        }
        let _restore = Restore(unsafe { GetForegroundWindow() }, original_pointer);
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
            let mut anchor = POINT::default();
            GetCursorPos(&mut anchor).unwrap();
            activate_for_test(taskbar_at(anchor).expect("barra de tareas"));
        }
        let (preview, values) = std::sync::mpsc::channel();
        let driver = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let root = loop {
                let hwnd = HWND(MENU_HWND.load(Ordering::SeqCst) as _);
                if !hwnd.is_invalid() && unsafe { IsWindowVisible(hwnd).as_bool() } {
                    break hwnd;
                }
                assert!(Instant::now() < deadline, "el menú debe abrirse");
                std::thread::sleep(Duration::from_millis(10));
            };
            struct Close(HWND);
            impl Drop for Close {
                fn drop(&mut self) {
                    unsafe {
                        let _ = PostMessageW(Some(self.0), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
            }
            let _close = Close(root);
            let shell = HWND(MENU_FOCUS.load(Ordering::SeqCst) as _);
            assert!(
                !shell.is_invalid(),
                "el menú de bandeja debe conservar el foco en Explorer"
            );
            assert!(foreground_is(shell));
            let s = unsafe { GetDpiForWindow(root) } as f32 / 96.0;
            let point =
                |x: f32, y: f32| LPARAM(((x * s) as u32 | (((y * s) as u32) << 16)) as isize);
            unsafe {
                SendMessageW(
                    root,
                    WM_LBUTTONUP,
                    None,
                    Some(point(40.0, PAD + ROW_H / 2.0)),
                );
            }
            let hwnd = HWND(MENU_KEYBOARD.load(Ordering::SeqCst) as _);
            assert_ne!(hwnd, root, "abrir el submenú de desenfoque");
            assert_eq!(
                unsafe { SendMessageW(hwnd, WM_MOUSEACTIVATE, None, None) }.0,
                MA_NOACTIVATE as isize
            );
            std::thread::sleep(Duration::from_millis(150));
            for _ in 0..2 {
                unsafe {
                    SendMessageW(
                        hwnd,
                        WM_LBUTTONUP,
                        Some(WPARAM(0)),
                        Some(point(40.0, PAD + ROW_H / 2.0)),
                    );
                }
                assert_eq!(MENU_KEYBOARD.load(Ordering::SeqCst), hwnd.0 as isize);
            }
            let y = PAD + ROW_H + SLIDER_H / 2.0;
            unsafe {
                SendMessageW(hwnd, WM_LBUTTONDOWN, Some(WPARAM(1)), Some(point(60.0, y)));
                let mut start = POINT::default();
                GetCursorPos(&mut start).unwrap();
                let mut monitor = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                assert!(GetMonitorInfoW(
                    MonitorFromPoint(start, MONITOR_DEFAULTTONEAREST),
                    &mut monitor
                )
                .as_bool());
                let dx = if start.x + 100 < monitor.rcMonitor.right {
                    20
                } else {
                    -20
                };
                let move_mouse = INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dx,
                            dwFlags: MOUSEEVENTF_MOVE,
                            ..Default::default()
                        },
                    },
                };
                assert_eq!(
                    SendInput(&[move_mouse], std::mem::size_of::<INPUT>() as i32),
                    1
                );
                let deadline = Instant::now() + Duration::from_millis(500);
                loop {
                    let mut pointer = POINT::default();
                    GetCursorPos(&mut pointer).unwrap();
                    if (pointer.x - start.x).signum() == dx.signum() {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "el puntero debe moverse durante el arrastre"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                let mut rect = RECT::default();
                GetWindowRect(hwnd, &mut rect).unwrap();
                let info = MSLLHOOKSTRUCT {
                    pt: POINT {
                        x: rect.right + 100,
                        y: rect.top + (y * s) as i32,
                    },
                    ..Default::default()
                };
                let info = LPARAM((&info as *const MSLLHOOKSTRUCT) as isize);
                assert_eq!(mouse_hook(0, WPARAM(WM_MOUSEMOVE as usize), info).0, 0);
                let deadline = Instant::now() + Duration::from_secs(1);
                while values
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("actualizar el desenfoque sin soltar el clic")
                    != 40
                {}
                assert_eq!(SLIDER_HWND.load(Ordering::SeqCst), hwnd.0 as isize);
                assert_eq!(mouse_hook(0, WPARAM(WM_LBUTTONUP as usize), info).0, 1);
                let deadline = Instant::now() + Duration::from_secs(1);
                while SLIDER_HWND.load(Ordering::SeqCst) != 0 {
                    assert!(
                        Instant::now() < deadline,
                        "terminar el arrastre al soltar fuera"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                // El submenú sigue recibiendo clics después de soltar fuera.
                SendMessageW(
                    hwnd,
                    WM_LBUTTONUP,
                    None,
                    Some(point(40.0, PAD + ROW_H / 2.0)),
                );
            }
            assert!(foreground_is(shell));
            assert_eq!(MENU_HWND.load(Ordering::SeqCst), root.0 as isize);
            assert!(unsafe { IsWindowVisible(hwnd).as_bool() });
        });
        let mut toggles = 0;
        let mut value = 16;
        let items = |checked, value| {
            vec![Item::Group {
                label: "Desenfoque del fondo".into(),
                items: vec![
                    Item::Action {
                        id: 1,
                        label: "Prueba".into(),
                        checked,
                    },
                    Item::Slider {
                        id: 500,
                        label: format!("Intensidad: {value}"),
                        value,
                        min: 0,
                        max: 40,
                    },
                ],
            }]
        };
        show_tray(items(false, value), |id| {
            if id == 1 {
                toggles += 1;
                if toggles == 1 {
                    // Windows puede despachar otro clic dentro de una llamada COM del
                    // callback. Debe esperar a que se libere State, no desaparecer.
                    let hwnd = HWND(MENU_KEYBOARD.load(Ordering::SeqCst) as _);
                    let s = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
                    let y = ((PAD + ROW_H / 2.0) * s) as u32;
                    unsafe {
                        SendMessageW(
                            hwnd,
                            WM_LBUTTONUP,
                            Some(WPARAM(0)),
                            Some(LPARAM(((y << 16) | (40.0 * s) as u32) as isize)),
                        );
                    }
                }
            } else {
                value = (id - 500) as u32;
                let _ = preview.send(value);
                return MenuAction::UpdateSlider {
                    id: 500,
                    value,
                    label: format!("Intensidad: {value}"),
                    checked: Some((1, toggles % 2 != 0)),
                };
            }
            MenuAction::KeepOpen(items(toggles % 2 != 0, value))
        });
        driver.join().unwrap();
        assert_eq!(toggles, 4);
        assert_eq!(value, 40);
    }
}
