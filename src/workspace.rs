//! Pestañas y acciones del panel principal, compartiendo geometría para dibujo y clics.
use crate::island::{self, Hit, LabelArea, NoticeIcon};
use tiny_skia::{Paint, Pixmap};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Music,
    Share,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum ShareAction {
    Files = 1,
    Clipboard = 2,
    Receive = 3,
    Pair = 4,
    Devices = 5,
}

#[derive(Default)]
pub struct ShareState {
    pub receiving: bool,
    pub devices: usize,
    pub busy: bool,
    pub status: String,
}

fn regions(width: f32, page: Page) -> Vec<(Hit, [f32; 4])> {
    let mut regions = vec![
        (Hit::Tab(Page::Music), [12.0, 8.0, 98.0, 26.0]),
        (Hit::Tab(Page::Share), [116.0, 8.0, 124.0, 26.0]),
    ];
    if page == Page::Share {
        regions.extend([
            (
                Hit::Share(ShareAction::Files),
                [14.0, 48.0, width - 28.0, 38.0],
            ),
            (
                Hit::Share(ShareAction::Clipboard),
                [14.0, 92.0, width - 28.0, 38.0],
            ),
            (Hit::Share(ShareAction::Pair), [14.0, 138.0, 90.0, 26.0]),
            (Hit::Share(ShareAction::Devices), [110.0, 138.0, 90.0, 26.0]),
            (
                Hit::Share(ShareAction::Receive),
                [width - 46.0, 138.0, 32.0, 26.0],
            ),
        ]);
    }
    regions
}

pub fn hit_test(
    bounds: (f32, f32, f32, f32, f32),
    scale: f32,
    page: Page,
    x: f32,
    y: f32,
) -> Option<Hit> {
    let (left, top, width, _, _) = bounds;
    let (x, y) = ((x - left) / scale, (y - top) / scale);
    regions(width / scale, page)
        .into_iter()
        .find(|(_, [rx, ry, w, h])| x >= *rx && x <= rx + w && y >= *ry && y <= ry + h)
        .map(|(hit, _)| hit)
}

#[allow(clippy::too_many_arguments)]
pub fn draw(
    pixmap: &mut Pixmap,
    bounds: (f32, f32, f32, f32, f32),
    scale: f32,
    alpha: f32,
    page: Page,
    hovered: Option<Hit>,
    state: &ShareState,
) -> Vec<LabelArea> {
    let (left, top, width, _, _) = bounds;
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    let mut labels = Vec::new();
    for (hit, [x, y, w, h]) in regions(width / scale, page) {
        let active = matches!(hit, Hit::Tab(tab) if tab == page);
        let hover = hovered == Some(hit);
        let (r, g, b, opacity) = if active {
            (40, 88, 128, 210)
        } else if hover {
            (65, 68, 78, 240)
        } else if matches!(hit, Hit::Share(ShareAction::Files | ShareAction::Clipboard)) {
            (32, 36, 45, 240)
        } else {
            (25, 27, 33, 150)
        };
        paint.set_color_rgba8(r, g, b, (opacity as f32 * alpha) as u8);
        island::fill_rounded_rect(
            pixmap,
            &paint,
            left + x * scale,
            top + y * scale,
            w * scale,
            h * scale,
            9.0 * scale,
        );
        let (label, icon) = match hit {
            Hit::Tab(Page::Music) => ("Música".to_string(), None),
            Hit::Tab(Page::Share) => ("Compartir".to_string(), Some(NoticeIcon::Wireless)),
            Hit::Share(ShareAction::Files) => {
                ("Elegir archivos…".to_string(), Some(NoticeIcon::Usb))
            }
            Hit::Share(ShareAction::Clipboard) => (
                "Enviar portapapeles".to_string(),
                Some(NoticeIcon::Keyboard),
            ),
            Hit::Share(ShareAction::Pair) => ("Vincular…".to_string(), None),
            Hit::Share(ShareAction::Devices) => (format!("Equipos · {}", state.devices), None),
            Hit::Share(ShareAction::Receive) => (String::new(), None),
            _ => continue,
        };
        if let Some(icon) = icon {
            paint.set_color_rgba8(116, 190, 252, (alpha * 255.0) as u8);
            island::draw_notice_icon(
                pixmap,
                &paint,
                icon,
                left + (x + 18.0) * scale,
                top + (y + h / 2.0) * scale,
                scale * 0.65,
            );
        }
        let inset = if icon.is_some() { 36.0 } else { 12.0 };
        let text = label;
        labels.push(LabelArea {
            text,
            x: left + (x + inset) * scale,
            y: top + (y + (h - 18.0) / 2.0) * scale,
            w: (w - inset - 10.0) * scale,
            h: 18.0 * scale,
            alpha,
            strong: active,
            scroll: false,
        });
        if hit == Hit::Share(ShareAction::Receive) {
            paint.set_color_rgba8(
                if state.receiving { 88 } else { 115 },
                if state.receiving { 190 } else { 118 },
                if state.receiving { 142 } else { 125 },
                (alpha * 255.0) as u8,
            );
            island::draw_notice_icon(
                pixmap,
                &paint,
                NoticeIcon::Wireless,
                left + (x + w / 2.0) * scale,
                top + (y + h / 2.0) * scale,
                scale * 0.7,
            );
        }
    }
    if page == Page::Share {
        let status = if hovered == Some(Hit::Share(ShareAction::Receive)) {
            if state.receiving {
                "Pausar recepción"
            } else {
                "Activar recepción"
            }
        } else if !state.status.is_empty() {
            &state.status
        } else if state.busy {
            "Solicitud en curso…"
        } else if state.receiving {
            "Recibiendo de tus equipos"
        } else {
            "Recepción pausada"
        };
        labels.push(LabelArea {
            text: status.to_string(),
            x: left + 18.0 * scale,
            y: top + 171.0 * scale,
            w: width - 36.0 * scale,
            h: 16.0 * scale,
            alpha: alpha * 0.7,
            strong: false,
            scroll: true,
        });
    }
    labels
}
