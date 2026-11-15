//! Cabecera con pestañas y acciones del panel principal, compartiendo geometría para dibujo
//! y clics.
//!
//! La cabecera (Compartir y Música, con una línea bajo la pestaña activa) va arriba en todas
//! las páginas; el contenido de cada página empieza debajo de `HEADER_HEIGHT`.
use crate::island::{self, Hit, LabelArea, NoticeIcon};
use tiny_skia::{PathBuilder, Pixmap};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Music,
    Share,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareAction {
    Files = 1,
    Clipboard = 2,
    Device,
    Inbox,
    Receiving,
}

#[derive(Default)]
pub struct ShareState {
    pub receiving: bool,
    pub device_name: String,
    pub busy: bool,
    pub status: String,
    pub unread: usize,
}

/// Margen lateral del contenido.
const PAD: f32 = 16.0;
const TAB_Y: f32 = 8.0;
const TAB_HEIGHT: f32 = 30.0;
/// Línea fina que separa la cabecera del contenido (el volumen se dibuja sobre ella).
const SEPARATOR_Y: f32 = 42.0;
/// Alto lógico de la cabecera: el contenido de las páginas empieza aquí.
pub const HEADER_HEIGHT: f32 = 46.0;

const DEVICE_Y: f32 = 52.0;
const DEVICE_H: f32 = 34.0;
const CARD_Y: f32 = DEVICE_Y + DEVICE_H + 8.0;
const CARD_H: f32 = 62.0;
const ROW_Y: f32 = CARD_Y + CARD_H + 8.0;
const ROW_H: f32 = 32.0;
const STATUS_Y: f32 = ROW_Y + ROW_H + 8.0;
pub const SHARE_HEIGHT: f32 = STATUS_Y + 24.0;

/// Rectángulo lógico de cada pestaña: primero Compartir y después Música.
fn tab_rect(page: Page) -> [f32; 4] {
    match page {
        Page::Share => [PAD, TAB_Y, 106.0, TAB_HEIGHT],
        Page::Music => [PAD + 110.0, TAB_Y, 90.0, TAB_HEIGHT],
    }
}

/// Centro lógico de una pestaña, relativo a la esquina de la isla.
#[cfg(test)]
pub fn tab_center(page: Page) -> (f32, f32) {
    let [x, y, w, h] = tab_rect(page);
    (x + w / 2.0, y + h / 2.0)
}

/// Centro vertical lógico de la cabecera; ahí van también la batería y la actualización.
pub const fn header_center_y() -> f32 {
    TAB_Y + TAB_HEIGHT / 2.0
}

fn regions(width: f32, page: Page) -> Vec<(Hit, [f32; 4])> {
    let mut regions = vec![
        (Hit::Tab(Page::Share), tab_rect(Page::Share)),
        (Hit::Tab(Page::Music), tab_rect(Page::Music)),
    ];
    if page == Page::Share {
        let inner = width - 2.0 * PAD;
        let half = (inner - 8.0) / 2.0;
        let right = PAD + half + 8.0;
        regions.extend([
            (
                Hit::Share(ShareAction::Device),
                [PAD, DEVICE_Y, inner, DEVICE_H],
            ),
            (Hit::Share(ShareAction::Files), [PAD, CARD_Y, half, CARD_H]),
            (
                Hit::Share(ShareAction::Clipboard),
                [right, CARD_Y, half, CARD_H],
            ),
            (Hit::Share(ShareAction::Inbox), [PAD, ROW_Y, half, ROW_H]),
            (
                Hit::Share(ShareAction::Receiving),
                [right, ROW_Y, half, ROW_H],
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

/// Pinta en píxeles físicos a partir de medidas lógicas relativas a la esquina de la isla.
struct Canvas<'a> {
    pixmap: &'a mut Pixmap,
    left: f32,
    top: f32,
    scale: f32,
    alpha: f32,
}

impl Canvas<'_> {
    fn paint(&self, [r, g, b]: [u8; 3], opacity: f32) -> tiny_skia::Paint<'static> {
        let mut paint = tiny_skia::Paint {
            anti_alias: true,
            ..Default::default()
        };
        paint.set_color_rgba8(r, g, b, (opacity * self.alpha).clamp(0.0, 255.0) as u8);
        paint
    }

    fn fill(&mut self, rgb: [u8; 3], opacity: f32, [x, y, w, h]: [f32; 4], radius: f32) {
        let paint = self.paint(rgb, opacity);
        let s = self.scale;
        island::fill_rounded_rect(
            self.pixmap,
            &paint,
            self.left + x * s,
            self.top + y * s,
            w * s,
            h * s,
            radius * s,
        );
    }

    fn outline(&mut self, rgb: [u8; 3], opacity: f32, [x, y, w, h]: [f32; 4], radius: f32) {
        let paint = self.paint(rgb, opacity);
        let s = self.scale;
        if let Some(path) = island::rounded_rect_path(
            self.left + x * s + 0.5,
            self.top + y * s + 0.5,
            (w * s - 1.0).max(1.0),
            (h * s - 1.0).max(1.0),
            (radius * s - 0.5).max(0.0),
        ) {
            island::stroke_path(self.pixmap, &paint, &path, s.max(1.0));
        }
    }

    fn icon(&mut self, icon: NoticeIcon, rgb: [u8; 3], cx: f32, cy: f32, size: f32) {
        let paint = self.paint(rgb, 255.0);
        let s = self.scale;
        island::draw_notice_icon(
            self.pixmap,
            &paint,
            icon,
            self.left + cx * s,
            self.top + cy * s,
            s * size,
        );
    }

    /// Flecha hacia abajo del selector de dispositivo.
    fn chevron(&mut self, rgb: [u8; 3], cx: f32, cy: f32) {
        let paint = self.paint(rgb, 220.0);
        let s = self.scale;
        let (x, y) = (self.left + cx * s, self.top + cy * s);
        let mut pb = PathBuilder::new();
        pb.move_to(x - 4.5 * s, y - 2.2 * s);
        pb.line_to(x, y + 2.3 * s);
        pb.line_to(x + 4.5 * s, y - 2.2 * s);
        island::stroke(self.pixmap, &paint, pb, 1.6 * s);
    }

    #[allow(clippy::too_many_arguments)]
    fn label(
        &self,
        text: String,
        [x, y, w, h]: [f32; 4],
        alpha: f32,
        strong: bool,
        scroll: bool,
    ) -> LabelArea {
        let s = self.scale;
        LabelArea {
            text,
            x: self.left + x * s,
            y: self.top + y * s,
            w: (w * s).max(0.0),
            h: h * s,
            alpha: self.alpha * alpha,
            strong,
            scroll,
        }
    }
}

const WHITE: [u8; 3] = [255, 255, 255];

#[allow(clippy::too_many_arguments)]
pub fn draw(
    pixmap: &mut Pixmap,
    bounds: (f32, f32, f32, f32, f32),
    scale: f32,
    alpha: f32,
    page: Page,
    hovered: Option<Hit>,
    state: &ShareState,
    color: crate::settings::IslandColor,
) -> Vec<LabelArea> {
    let (left, top, width, _, _) = bounds;
    let width = width / scale;
    let accent = color.light();
    let mut canvas = Canvas {
        pixmap,
        left,
        top,
        scale,
        alpha,
    };
    let mut labels = Vec::new();

    // Separador de la cabecera, de borde a borde del contenido.
    canvas.fill(WHITE, 22.0, [PAD, SEPARATOR_Y, width - 2.0 * PAD, 1.0], 0.5);

    for (hit, rect) in regions(width, page) {
        let hover = hovered == Some(hit);
        let [x, y, w, h] = rect;
        match hit {
            Hit::Tab(tab) => {
                let active = tab == page;
                if hover && !active {
                    canvas.fill(WHITE, 14.0, rect, 9.0);
                }
                if active {
                    canvas.fill(
                        accent,
                        255.0,
                        [x + 6.0, SEPARATOR_Y - 2.5, w - 12.0, 2.5],
                        1.25,
                    );
                }
                let (icon, text) = match tab {
                    Page::Music => (NoticeIcon::Music, "Música"),
                    Page::Share => (NoticeIcon::Share, "Compartir"),
                };
                let tone = if active { WHITE } else { accent };
                canvas.icon(icon, tone, x + 16.0, y + h / 2.0, 0.8);
                labels.push(canvas.label(
                    text.to_string(),
                    [x + 32.0, y + (h - 18.0) / 2.0, w - 34.0, 18.0],
                    if active { 1.0 } else { 0.62 },
                    active,
                    false,
                ));
            }
            Hit::Share(ShareAction::Device) => {
                canvas.fill(WHITE, if hover { 30.0 } else { 18.0 }, rect, 12.0);
                canvas.outline(WHITE, 20.0, rect, 12.0);
                canvas.icon(NoticeIcon::Wireless, WHITE, x + 22.0, y + h / 2.0, 0.72);
                let text = if !state.status.is_empty() {
                    state.status.clone()
                } else if state.busy {
                    "Solicitud en curso…".to_string()
                } else if state.device_name.is_empty() {
                    "Para: vincula un dispositivo…".to_string()
                } else {
                    format!("Para: {}", state.device_name)
                };
                labels.push(canvas.label(
                    text,
                    [x + 42.0, y + (h - 18.0) / 2.0, w - 72.0, 18.0],
                    1.0,
                    false,
                    true,
                ));
                canvas.chevron(WHITE, x + w - 18.0, y + h / 2.0);
            }
            Hit::Share(action @ (ShareAction::Files | ShareAction::Clipboard)) => {
                // Elegir archivos es la acción principal: va teñida con el color de la isla.
                let primary = action == ShareAction::Files;
                let (fill, border) = if primary {
                    (accent, if hover { 150.0 } else { 105.0 })
                } else {
                    (WHITE, if hover { 46.0 } else { 26.0 })
                };
                canvas.fill(
                    fill,
                    match (primary, hover) {
                        (true, true) => 62.0,
                        (true, false) => 44.0,
                        (false, true) => 30.0,
                        (false, false) => 18.0,
                    },
                    rect,
                    14.0,
                );
                canvas.outline(if primary { accent } else { WHITE }, border, rect, 14.0);
                let icon_box = [x + 12.0, y + (h - 34.0) / 2.0, 34.0, 34.0];
                canvas.fill(if primary { accent } else { WHITE }, 46.0, icon_box, 10.0);
                canvas.outline(WHITE, 30.0, icon_box, 10.0);
                let (icon, first, second) = match action {
                    ShareAction::Files => (NoticeIcon::Usb, "Elegir", "archivos"),
                    _ => (NoticeIcon::Keyboard, "Enviar", "portapapeles"),
                };
                canvas.icon(icon, WHITE, x + 29.0, y + h / 2.0, 0.72);
                let text_x = x + 54.0;
                let text_w = w - 58.0;
                labels.push(canvas.label(
                    first.to_string(),
                    [text_x, y + h / 2.0 - 18.0, text_w, 18.0],
                    1.0,
                    true,
                    false,
                ));
                labels.push(canvas.label(
                    second.to_string(),
                    [text_x, y + h / 2.0, text_w, 18.0],
                    1.0,
                    true,
                    true,
                ));
            }
            Hit::Share(ShareAction::Inbox) => {
                canvas.fill(WHITE, if hover { 30.0 } else { 16.0 }, rect, 12.0);
                canvas.outline(WHITE, 20.0, rect, 12.0);
                canvas.icon(NoticeIcon::Share, WHITE, x + 18.0, y + h / 2.0, 0.66);
                let text = match state.unread {
                    0 => "Mensajes recibidos".to_string(),
                    1 => "1 mensaje sin leer".to_string(),
                    n => format!("{n} mensajes sin leer"),
                };
                if state.unread > 0 {
                    canvas.fill(accent, 255.0, [x + 25.0, y + 7.0, 6.0, 6.0], 3.0);
                }
                labels.push(canvas.label(
                    text,
                    [x + 34.0, y + (h - 18.0) / 2.0, w - 40.0, 18.0],
                    1.0,
                    false,
                    true,
                ));
            }
            Hit::Share(ShareAction::Receiving) => {
                let on = state.receiving;
                canvas.fill(WHITE, if hover { 30.0 } else { 16.0 }, rect, 12.0);
                canvas.outline(
                    if on { accent } else { WHITE },
                    if on { 120.0 } else { 20.0 },
                    rect,
                    12.0,
                );
                canvas.icon(NoticeIcon::Wireless, WHITE, x + 18.0, y + h / 2.0, 0.66);
                let text = if on {
                    "Recepción activa"
                } else {
                    "No molestar"
                };
                if on {
                    canvas.fill(
                        accent,
                        255.0,
                        [x + w - 18.0, y + h / 2.0 - 4.0, 8.0, 8.0],
                        4.0,
                    );
                }
                labels.push(canvas.label(
                    text.to_string(),
                    [x + 34.0, y + (h - 18.0) / 2.0, w - 58.0, 18.0],
                    if on { 1.0 } else { 0.7 },
                    false,
                    true,
                ));
            }
            _ => {}
        }
    }
    if page == Page::Share {
        let text = if state.receiving {
            "Tus equipos vinculados pueden enviarte archivos"
        } else {
            "Recepción pausada: no se aceptan envíos"
        };
        labels.push(canvas.label(
            text.to_string(),
            [PAD + 4.0, STATUS_Y, width - 2.0 * PAD - 8.0, 16.0],
            0.55,
            false,
            true,
        ));
    }
    labels
}

/// Nivel de volumen sobre el separador de la cabecera, mientras se cambia.
pub fn draw_volume(
    pixmap: &mut Pixmap,
    bounds: (f32, f32, f32, f32, f32),
    scale: f32,
    alpha: f32,
    level: f32,
    color: crate::settings::IslandColor,
) {
    let (left, top, width, _, _) = bounds;
    let mut canvas = Canvas {
        pixmap,
        left,
        top,
        scale,
        alpha,
    };
    let w = width / scale - 2.0 * PAD;
    let y = SEPARATOR_Y - 1.0;
    canvas.fill(WHITE, 55.0, [PAD, y, w, 3.0], 1.5);
    canvas.fill(
        color.light(),
        255.0,
        [PAD, y, w * level.clamp(0.0, 1.0), 3.0],
        1.5,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pestañas_y_acciones_coinciden_a_distintos_dpi() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let width = island::EXPANDED_COMPACT.0;
            let bounds = (80.0, 8.0, width * scale, SHARE_HEIGHT * scale, 24.0 * scale);
            let regions = regions(width, Page::Share);
            for (index, (_, [x, y, w, h])) in regions.iter().enumerate() {
                assert!(*x >= 16.0 && x + w <= width - 16.0);
                assert!(*y >= 8.0 && y + h <= SHARE_HEIGHT - 12.0);
                for (_, [ox, oy, ow, oh]) in &regions[index + 1..] {
                    assert!(x + w <= *ox || ox + ow <= *x || y + h <= *oy || oy + oh <= *y);
                }
            }
            for (hit, [x, y, w, h]) in regions {
                assert!(
                    hit_test(
                        bounds,
                        scale,
                        Page::Share,
                        bounds.0 + (x + w / 2.0) * scale,
                        bounds.1 + (y + h / 2.0) * scale
                    ) == Some(hit)
                );
            }
            // En la música solo responden las pestañas: el resto es del reproductor.
            assert!(hit_test(
                bounds,
                scale,
                Page::Music,
                bounds.0 + 60.0 * scale,
                bounds.1 + 100.0 * scale
            )
            .is_none());
            let (mx, my) = tab_center(Page::Music);
            assert!(
                hit_test(
                    bounds,
                    scale,
                    Page::Music,
                    bounds.0 + mx * scale,
                    bounds.1 + my * scale
                ) == Some(Hit::Tab(Page::Music))
            );
            let mut image = Pixmap::new(640, 400).unwrap();
            draw_volume(
                &mut image,
                bounds,
                scale,
                1.0,
                0.5,
                crate::settings::IslandColor::default(),
            );
            let y = (bounds.1 + SEPARATOR_Y * scale) as u32;
            let inner = width - 2.0 * PAD;
            for (x, expected) in [
                (PAD - 4.0, 0),
                (PAD + inner * 0.25, 255),
                (PAD + inner * 0.75, 55),
                (width - PAD + 4.0, 0),
            ] {
                let x = (bounds.0 + x * scale) as u32;
                assert_eq!(image.pixel(x, y).unwrap().alpha(), expected);
            }
        }
    }

    #[test]
    fn compartir_muestra_estado_y_bandeja_legibles() {
        let mut image = Pixmap::new(400, 260).unwrap();
        let width = island::EXPANDED_COMPACT.0;
        let labels = draw(
            &mut image,
            (0.0, 0.0, width, SHARE_HEIGHT, 24.0),
            1.0,
            1.0,
            Page::Share,
            None,
            &ShareState {
                receiving: true,
                unread: 2,
                device_name: "Portátil".into(),
                ..Default::default()
            },
            crate::settings::IslandColor::default(),
        );
        let has = |labels: &[LabelArea], text: &str| labels.iter().any(|label| label.text == text);
        assert!(has(&labels, "Recepción activa"));
        assert!(has(&labels, "2 mensajes sin leer"));
        assert!(has(&labels, "Para: Portátil"));
        assert!(has(&labels, "Elegir") && has(&labels, "archivos"));
        assert!(has(&labels, "Enviar") && has(&labels, "portapapeles"));
        assert!(has(&labels, "Compartir") && has(&labels, "Música"));
        for label in &labels {
            assert!(
                label.x >= 0.0 && label.x + label.w <= width,
                "{}",
                label.text
            );
            assert!(
                label.y >= 0.0 && label.y + label.h <= SHARE_HEIGHT,
                "{}",
                label.text
            );
        }

        let paused = draw(
            &mut image,
            (0.0, 0.0, width, SHARE_HEIGHT, 24.0),
            1.0,
            1.0,
            Page::Share,
            None,
            &ShareState {
                status: "Enviando a Portátil…".into(),
                ..Default::default()
            },
            crate::settings::IslandColor::default(),
        );
        assert!(has(&paused, "No molestar"));
        assert!(has(&paused, "Enviando a Portátil…"));
    }

    #[test]
    fn la_pestaña_activa_se_subraya() {
        for page in [Page::Music, Page::Share] {
            let mut image = Pixmap::new(400, 120).unwrap();
            let width = island::EXPANDED_COMPACT.0;
            draw(
                &mut image,
                (0.0, 0.0, width, SHARE_HEIGHT, 24.0),
                1.0,
                1.0,
                page,
                None,
                &ShareState::default(),
                crate::settings::IslandColor::default(),
            );
            let y = (SEPARATOR_Y - 1.5) as u32;
            for tab in [Page::Music, Page::Share] {
                let (cx, _) = tab_center(tab);
                let alpha = image.pixel(cx as u32, y).unwrap().alpha();
                assert_eq!(alpha == 255, tab == page, "{tab:?} en {page:?}");
            }
        }
    }
}
