//! Configuración por categorías. Solo dibuja al recibir entrada o un cambio de estado.
//! El bucle despacha los mensajes de la isla; los comandos se ejecutan sin préstamos de App.
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicIsize, Ordering};

use tiny_skia::{Color, Paint};
use windows::core::{w, Result};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::island::{self, LabelArea, LayoutAreas, TextArea};
use crate::menu::Item;
use crate::text::TextRenderer;
use crate::Surface;

pub struct Category {
    pub title: String,
    pub description: String,
    pub items: Vec<Item>,
}

static WINDOW: AtomicIsize = AtomicIsize::new(0);

const WIDTH: f32 = 720.0;
const HEIGHT: f32 = 548.0;
const CONTENT_X: f32 = 208.0;
const CONTENT_Y: f32 = 132.0;
const CONTENT_BOTTOM: f32 = 500.0;

enum Event {
    Down(f32, f32),
    Move(f32),
    Up,
    Wheel(f32),
    Key(u32, bool),
    Dpi,
    Close,
}

thread_local! {
    static EVENTS: RefCell<VecDeque<Event>> = const { RefCell::new(VecDeque::new()) };
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let x = (lp.0 as u16 as i16) as f32;
    let y = ((lp.0 >> 16) as u16 as i16) as f32;
    let event = match msg {
        WM_LBUTTONDOWN => Some(Event::Down(x, y)),
        WM_MOUSEMOVE => Some(Event::Move(x)),
        WM_LBUTTONUP | WM_CAPTURECHANGED => Some(Event::Up),
        WM_MOUSEWHEEL => Some(Event::Wheel(((wp.0 >> 16) as u16 as i16) as f32)),
        WM_KEYDOWN => Some(Event::Key(wp.0 as u32, unsafe {
            windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x10) < 0
        })),
        WM_DPICHANGED => Some(Event::Dpi),
        WM_CLOSE => Some(Event::Close),
        WM_ERASEBKGND => return LRESULT(1),
        _ => None,
    };
    if let Some(event) = event {
        EVENTS.with_borrow_mut(|events| {
            if matches!(event, Event::Move(_)) && matches!(events.back(), Some(Event::Move(_))) {
                events.pop_back();
            }
            events.push_back(event);
        });
        LRESULT(0)
    } else {
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }
}

#[derive(Clone)]
struct Row {
    item: Item,
    top: f32,
    height: f32,
}

fn rows(items: &[Item]) -> Vec<Row> {
    fn append(items: &[Item], out: &mut Vec<Row>, y: &mut f32) {
        for item in items {
            if let Item::Group { label, items } = item {
                if !matches!(items.first(), Some(Item::Label(title)) if title == label) {
                    append(&[Item::Label(label.clone())], out, y);
                }
                append(items, out, y);
                continue;
            }
            let height = match item {
                Item::Separator => 12.0,
                Item::Label(_) => 30.0,
                Item::Slider { .. } => 72.0,
                Item::DeviceStatus { .. } => 56.0,
                _ => 44.0,
            };
            out.push(Row {
                item: item.clone(),
                top: *y,
                height,
            });
            *y += height;
        }
    }
    let mut out = Vec::new();
    append(items, &mut out, &mut 0.0);
    out
}

struct Panel {
    hwnd: HWND,
    surface: Surface,
    text: TextRenderer,
    scale: f32,
    categories: Vec<Category>,
    category: usize,
    scroll: f32,
    focus: usize,
    dragging: Option<usize>,
}

impl Panel {
    fn rows(&self) -> Vec<Row> {
        rows(&self.categories[self.category].items)
    }

    fn max_scroll(&self) -> f32 {
        self.rows().last().map_or(0.0, |r| {
            (r.top + r.height - (CONTENT_BOTTOM - CONTENT_Y)).max(0.0)
        })
    }

    fn selectable(&self) -> Vec<usize> {
        self.rows()
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                matches!(row.item, Item::Action { .. } | Item::Slider { .. }).then_some(i)
            })
            .collect()
    }

    fn select_category(&mut self, index: usize) {
        self.category = index;
        self.scroll = 0.0;
        self.focus = 0;
    }

    fn focused_row(&self) -> Option<usize> {
        self.focus
            .checked_sub(self.categories.len())
            .and_then(|i| self.selectable().get(i).copied())
    }

    fn reveal_focus(&mut self) {
        if let Some(index) = self.focused_row() {
            let row = &self.rows()[index];
            let visible = CONTENT_BOTTOM - CONTENT_Y;
            if row.top < self.scroll {
                self.scroll = row.top;
            }
            if row.top + row.height > self.scroll + visible {
                self.scroll = row.top + row.height - visible;
            }
        }
    }

    fn slider_command(&self, index: usize, x: f32) -> Option<usize> {
        let row = self.rows().get(index)?.clone();
        if let Item::Slider {
            id,
            value,
            min,
            max,
            ..
        } = row.item
        {
            let fraction = ((x - CONTENT_X - 16.0) / (WIDTH - CONTENT_X - 64.0)).clamp(0.0, 1.0);
            let next = (min as f32 + fraction * (max - min) as f32).round() as u32;
            (next != value).then_some(id + next as usize)
        } else {
            None
        }
    }

    fn rescale(&mut self) -> Result<()> {
        unsafe {
            let monitor = windows::Win32::Graphics::Gdi::MonitorFromWindow(
                self.hwnd,
                MONITOR_DEFAULTTONEAREST,
            );
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let _ = GetMonitorInfoW(monitor, &mut info);
            let (mut dx, mut dy) = (96, 96);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let scale = (dx as f32 / 96.0)
                .min((info.rcWork.right - info.rcWork.left - 24).max(1) as f32 / WIDTH)
                .min((info.rcWork.bottom - info.rcWork.top - 24).max(1) as f32 / HEIGHT);
            if (scale - self.scale).abs() > 0.001 {
                let width = (WIDTH * scale).ceil() as u32;
                let height = (HEIGHT * scale).ceil() as u32;
                let surface = Surface::new(width, height)?;
                let text = TextRenderer::with_label_capacity(scale, 40)?;
                self.surface = surface;
                self.text = text;
                self.scale = scale;
            }
        }
        Ok(())
    }

    fn render(&mut self) {
        let s = self.scale;
        let mut labels = Vec::new();
        let mut paint = Paint {
            anti_alias: true,
            ..Default::default()
        };
        self.surface.pixmap.fill(Color::TRANSPARENT);
        macro_rules! rect {
            ($x:expr, $y:expr, $w:expr, $h:expr, $r:expr, $color:expr) => {{
                let [r, g, b, a] = $color;
                paint.set_color_rgba8(r, g, b, a);
                island::fill_rounded_rect(
                    &mut self.surface.pixmap,
                    &paint,
                    $x * s,
                    $y * s,
                    $w * s,
                    $h * s,
                    $r * s,
                );
            }};
        }
        macro_rules! label {
            ($text:expr, $x:expr, $y:expr, $w:expr, $strong:expr, $alpha:expr) => {
                labels.push(LabelArea {
                    text: $text.to_string(),
                    x: $x * s,
                    y: $y * s,
                    w: $w * s,
                    h: 20.0 * s,
                    strong: $strong,
                    alpha: $alpha,
                    scroll: false,
                });
            };
        }
        rect!(0.0, 0.0, WIDTH, HEIGHT, 18.0, [12, 18, 30, 255]);
        rect!(12.0, 76.0, 172.0, HEIGHT - 88.0, 12.0, [19, 28, 44, 255]);
        rect!(WIDTH - 48.0, 18.0, 28.0, 28.0, 8.0, [39, 51, 70, 255]);
        label!("×", WIDTH - 39.0, 22.0, 18.0, true, 1.0);
        for (i, category) in self.categories.iter().enumerate() {
            let y = 88.0 + i as f32 * 42.0;
            if i == self.category {
                rect!(20.0, y, 156.0, 36.0, 9.0, [37, 73, 124, 255]);
            }
            if self.focus == i {
                rect!(24.0, y + 9.0, 3.0, 18.0, 1.5, [134, 193, 255, 255]);
            }
            label!(
                &category.title,
                34.0,
                y + 8.0,
                136.0,
                i == self.category,
                1.0
            );
        }
        let category = &self.categories[self.category];
        label!(&category.title, CONTENT_X, 78.0, 440.0, true, 1.0);
        label!(&category.description, CONTENT_X, 102.0, 456.0, false, 0.65);
        let focus_row = self.focused_row();
        for (index, row) in self.rows().iter().enumerate() {
            let y = CONTENT_Y + row.top - self.scroll;
            // Filas enteras: no se dibujan etiquetas ni controles debajo de la cabecera o el pie.
            if y < CONTENT_Y || y + row.height > CONTENT_BOTTOM {
                continue;
            }
            if matches!(
                row.item,
                Item::Action { .. } | Item::Slider { .. } | Item::DeviceStatus { .. }
            ) {
                rect!(
                    CONTENT_X,
                    y,
                    WIDTH - CONTENT_X - 24.0,
                    row.height - 5.0,
                    9.0,
                    if focus_row == Some(index) {
                        [36, 54, 80, 255]
                    } else {
                        [23, 33, 50, 255]
                    }
                );
            }
            match &row.item {
                Item::Label(text) => {
                    label!(text, CONTENT_X + 2.0, y + 5.0, 460.0, true, 0.65);
                }
                Item::Action {
                    id,
                    label: text,
                    checked,
                } => {
                    label!(
                        text,
                        CONTENT_X + 14.0,
                        y + 10.0,
                        WIDTH - CONTENT_X - 112.0,
                        false,
                        1.0
                    );
                    if crate::tray::is_toggle(*id) {
                        let x = WIDTH - 78.0;
                        rect!(
                            x,
                            y + 11.0,
                            36.0,
                            18.0,
                            9.0,
                            if *checked {
                                [67, 135, 231, 255]
                            } else {
                                [64, 77, 96, 255]
                            }
                        );
                        rect!(
                            x + if *checked { 20.0 } else { 3.0 },
                            y + 14.0,
                            12.0,
                            12.0,
                            6.0,
                            [240, 246, 255, 255]
                        );
                    } else {
                        label!(
                            if *checked { "✓" } else { "›" },
                            WIDTH - 62.0,
                            y + 9.0,
                            24.0,
                            true,
                            0.85
                        );
                    }
                }
                Item::Slider {
                    label: text,
                    value,
                    min,
                    max,
                    ..
                } => {
                    label!(text, CONTENT_X + 14.0, y + 8.0, 430.0, false, 1.0);
                    let width = WIDTH - CONTENT_X - 64.0;
                    let fraction = (*value - *min) as f32 / (*max - *min).max(1) as f32;
                    rect!(
                        CONTENT_X + 16.0,
                        y + 46.0,
                        width,
                        4.0,
                        2.0,
                        [62, 78, 103, 255]
                    );
                    rect!(
                        CONTENT_X + 16.0,
                        y + 46.0,
                        (width * fraction).max(4.0),
                        4.0,
                        2.0,
                        [89, 165, 255, 255]
                    );
                    rect!(
                        CONTENT_X + 10.0 + width * fraction,
                        y + 42.0,
                        12.0,
                        12.0,
                        6.0,
                        [230, 242, 255, 255]
                    );
                }
                Item::DeviceStatus {
                    name, status, icon, ..
                } => {
                    paint.set_color_rgba8(134, 193, 255, 255);
                    island::draw_notice_icon(
                        &mut self.surface.pixmap,
                        &paint,
                        *icon,
                        (CONTENT_X + 25.0) * s,
                        (y + 24.0) * s,
                        s * 0.7,
                    );
                    label!(name, CONTENT_X + 48.0, y + 5.0, 405.0, false, 1.0);
                    label!(status, CONTENT_X + 48.0, y + 26.0, 405.0, false, 0.65);
                }
                _ => {}
            }
        }
        if self.max_scroll() > 0.0 {
            let range = CONTENT_BOTTOM - CONTENT_Y;
            let thumb = (range * range / (range + self.max_scroll())).max(28.0);
            let y = CONTENT_Y + (range - thumb) * self.scroll / self.max_scroll();
            rect!(WIDTH - 14.0, y, 3.0, thumb, 1.5, [77, 102, 138, 255]);
        }
        label!(
            "Los cambios se guardan automáticamente",
            CONTENT_X,
            518.0,
            440.0,
            false,
            0.55
        );
        let areas = LayoutAreas {
            labels,
            text: TextArea {
                x: 24.0 * s,
                w: 580.0 * s,
                cy: 38.0 * s,
                alpha: 1.0,
            },
            time: None,
            lyrics: None,
            seek: None,
        };
        self.surface.copy_to_dib();
        let (w, h) = (self.surface.width, self.surface.height);
        self.text.draw(
            self.surface.dib(),
            w,
            h,
            (0, 0, w, h),
            &areas,
            "Configuración",
            "Notch Isla",
            0.0,
        );
        unsafe {
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                None,
                Some(&SIZE {
                    cx: w as i32,
                    cy: h as i32,
                }),
                Some(self.surface.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }
}

/// Ventana independiente: permanece abierta al cambiar opciones y se libera por completo al cerrar.
pub fn show<F>(categories: Vec<Category>, mut command: F) -> Result<()>
where
    F: FnMut(Option<usize>) -> (Vec<Category>, bool),
{
    unsafe {
        let existing = WINDOW.load(Ordering::SeqCst);
        if existing != 0 {
            let hwnd = HWND(existing as *mut _);
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
            return Ok(());
        }
        let instance = GetModuleHandleW(None)?;
        let class = w!("NotchSettingsWindow");
        RegisterClassExW(&WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        });
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dx, mut dy) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let scale = (dx as f32 / 96.0)
            .min((info.rcWork.right - info.rcWork.left - 24).max(1) as f32 / WIDTH)
            .min((info.rcWork.bottom - info.rcWork.top - 24).max(1) as f32 / HEIGHT);
        let (width, height) = (
            (WIDTH * scale).ceil() as u32,
            (HEIGHT * scale).ceil() as u32,
        );
        let surface = Surface::new(width, height)?;
        let text = TextRenderer::with_label_capacity(scale, 40)?;
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_APPWINDOW,
            class,
            w!("Notch Isla · Configuración"),
            WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX,
            info.rcWork.left + (info.rcWork.right - info.rcWork.left - width as i32) / 2,
            info.rcWork.top + (info.rcWork.bottom - info.rcWork.top - height as i32) / 2,
            width as i32,
            height as i32,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        struct Close(HWND);
        impl Drop for Close {
            fn drop(&mut self) {
                WINDOW.store(0, Ordering::SeqCst);
                unsafe {
                    let _ = ReleaseCapture();
                    let _ = DestroyWindow(self.0);
                }
            }
        }
        WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);
        let _close = Close(hwnd);
        EVENTS.with_borrow_mut(|events| events.clear());
        let mut panel = Panel {
            hwnd,
            surface,
            text,
            scale,
            categories,
            category: 0,
            scroll: 0.0,
            focus: 0,
            dragging: None,
        };
        panel.render();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let mut msg = MSG::default();
        'messages: loop {
            let result = GetMessageW(&mut msg, None, 0, 0).0;
            if result <= 0 {
                if result == 0 {
                    PostQuitMessage(msg.wParam.0 as i32);
                }
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            crate::ui::promote_timers();
            if matches!(
                msg.message,
                crate::battery::WM_BATTERY
                    | crate::update::WM_UPDATE
                    | crate::update::WM_UPDATE_CHECKED
            ) {
                panel.categories = command(None).0;
                panel.scroll = panel.scroll.min(panel.max_scroll());
                panel.render();
            }
            while let Some(event) = EVENTS.with_borrow_mut(|events| events.pop_front()) {
                let old_category = panel.category;
                let mut id = None;
                match event {
                    Event::Close => break 'messages,
                    Event::Down(x, y) => {
                        let (x, y) = (x / panel.scale, y / panel.scale);
                        if x >= WIDTH - 48.0 && y < 58.0 {
                            break 'messages;
                        }
                        if y < 66.0 {
                            let _ = ReleaseCapture();
                            SendMessageW(
                                hwnd,
                                WM_NCLBUTTONDOWN,
                                Some(WPARAM(HTCAPTION as usize)),
                                Some(LPARAM(0)),
                            );
                        } else if (20.0..184.0).contains(&x) && y >= 88.0 {
                            let index = ((y - 88.0) / 42.0) as usize;
                            if index < panel.categories.len() {
                                panel.select_category(index);
                                panel.focus = index;
                            }
                        } else if x >= CONTENT_X && (CONTENT_Y..CONTENT_BOTTOM).contains(&y) {
                            for (index, row) in panel.rows().iter().enumerate() {
                                let top = CONTENT_Y + row.top - panel.scroll;
                                if top < CONTENT_Y || top + row.height > CONTENT_BOTTOM {
                                    continue;
                                }
                                if (top..top + row.height).contains(&y) {
                                    if let Some(i) =
                                        panel.selectable().iter().position(|r| *r == index)
                                    {
                                        panel.focus = panel.categories.len() + i;
                                    }
                                    match row.item {
                                        Item::Action { id: action, .. } => id = Some(action),
                                        Item::Slider { .. } => {
                                            panel.dragging = Some(index);
                                            SetCapture(hwnd);
                                            id = panel.slider_command(index, x);
                                        }
                                        _ => {}
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    Event::Move(x) => {
                        if let Some(index) = panel.dragging {
                            id = panel.slider_command(index, x / panel.scale);
                            if id.is_none() {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }
                    Event::Up => {
                        panel.dragging = None;
                        let _ = ReleaseCapture();
                    }
                    Event::Wheel(delta) => {
                        panel.scroll =
                            (panel.scroll - delta / 120.0 * 44.0).clamp(0.0, panel.max_scroll());
                    }
                    Event::Dpi => {
                        panel.rescale()?;
                    }
                    Event::Key(key, shift) => match key {
                        0x1B => break 'messages,
                        0x09 => {
                            let count = panel.categories.len() + panel.selectable().len();
                            panel.focus = if shift {
                                (panel.focus + count - 1) % count
                            } else {
                                (panel.focus + 1) % count
                            };
                            panel.reveal_focus();
                        }
                        0x26 | 0x28 if panel.focus < panel.categories.len() => {
                            let count = panel.categories.len();
                            let index =
                                (panel.focus + if key == 0x26 { count - 1 } else { 1 }) % count;
                            panel.select_category(index);
                            panel.focus = index;
                        }
                        0x25 | 0x27 => {
                            if let Some(index) = panel.focused_row() {
                                if let Item::Slider {
                                    id: action,
                                    value,
                                    min,
                                    max,
                                    ..
                                } = panel.rows()[index].item
                                {
                                    let value = if key == 0x25 {
                                        value.saturating_sub(1).max(min)
                                    } else {
                                        (value + 1).min(max)
                                    };
                                    id = Some(action + value as usize);
                                }
                            }
                        }
                        0x20 | 0x0D => {
                            if panel.focus < panel.categories.len() {
                                let index = panel.focus;
                                panel.select_category(index);
                                panel.focus = index;
                            } else if let Some(index) = panel.focused_row() {
                                if let Item::Action { id: action, .. } = panel.rows()[index].item {
                                    id = Some(action);
                                }
                            }
                        }
                        0x21 | 0x22 => {
                            panel.scroll = (panel.scroll + if key == 0x21 { -264.0 } else { 264.0 })
                                .clamp(0.0, panel.max_scroll())
                        }
                        _ => continue,
                    },
                }
                if old_category != panel.category {
                    panel.categories = command(None).0;
                }
                if let Some(id) = id {
                    let (categories, close) = command(Some(id));
                    if close {
                        break 'messages;
                    }
                    panel.categories = categories;
                    panel.scroll = panel.scroll.min(panel.max_scroll());
                    let count = panel.categories.len() + panel.selectable().len();
                    panel.focus = panel.focus.min(count.saturating_sub(1));
                }
                panel.render();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grupos_se_integran_sin_duplicar_titulos_y_conservan_las_acciones() {
        let items = vec![Item::Group {
            label: "Dispositivos y batería".into(),
            items: vec![
                Item::Label("Dispositivos y batería".into()),
                Item::Separator,
                Item::Action {
                    id: 1,
                    label: "Avisos".into(),
                    checked: true,
                },
                Item::Group {
                    label: "Detalles".into(),
                    items: vec![Item::Action {
                        id: 2,
                        label: "Actualizar".into(),
                        checked: false,
                    }],
                },
            ],
        }];
        let rows = rows(&items);
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(&row.item,
            Item::Label(label) if label == "Dispositivos y batería"))
                .count(),
            1
        );
        let ids: Vec<_> = rows
            .iter()
            .filter_map(|row| match row.item {
                Item::Action { id, .. } => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![1, 2]);
        for pair in rows.windows(2) {
            assert_eq!(pair[0].top + pair[0].height, pair[1].top);
        }
    }
}
