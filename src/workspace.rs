//! Pestañas y acciones del panel principal, compartiendo geometría para dibujo y clics.
use crate::island::{self, Hit, LabelArea, NoticeIcon};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};

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
}

#[derive(Default)]
pub struct ShareState {
    pub receiving: bool,
    pub devices: usize,
    pub device_name: String,
    pub device_available: bool,
    pub busy: bool,
    pub status: String,
}

const TAB_X: f32 = 16.0;
const TAB_Y: f32 = 108.0;
const EMPTY_TAB_Y: f32 = 16.0;
const TAB_WIDTH: f32 = 36.0;
const TAB_HEIGHT: f32 = 28.0;
pub const SHARE_HEIGHT: f32 = 184.0;
const SHARE_TAB_Y: f32 = SHARE_HEIGHT - 40.0;

pub fn share_tab_center_y() -> f32 {
    SHARE_TAB_Y + TAB_HEIGHT / 2.0
}

pub fn tab_center_y(empty_music: bool) -> f32 {
    (if empty_music { EMPTY_TAB_Y } else { TAB_Y }) + TAB_HEIGHT / 2.0
}

fn fill_tab(pixmap: &mut Pixmap, paint: &Paint, left: f32, top: f32, scale: f32, tab: Page) {
    let x = left + TAB_X * scale;
    let seam = (x + TAB_WIDTH * scale).round();
    let y = top.round();
    let bottom = (top + TAB_HEIGHT * scale).round();
    let (outer, inward) = match tab {
        Page::Music => (x.round(), 1.0),
        Page::Share => ((x + 2.0 * TAB_WIDTH * scale).round(), -1.0),
    };
    let radius = 10.0 * scale;
    let bend = radius * 0.552_284_8;
    let corner = outer + inward * radius;
    let control = corner - inward * bend;
    let mut path = PathBuilder::new();
    path.move_to(seam, y);
    path.line_to(corner, y);
    path.cubic_to(control, y, outer, y + radius - bend, outer, y + radius);
    path.line_to(outer, bottom - radius);
    path.cubic_to(
        outer,
        bottom - radius + bend,
        control,
        bottom,
        corner,
        bottom,
    );
    path.line_to(seam, bottom);
    path.close();
    if let Some(path) = path.finish() {
        pixmap.fill_path(&path, paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn regions(width: f32, page: Page, empty_music: bool) -> Vec<(Hit, [f32; 4])> {
    let tab_y = if page == Page::Share {
        share_tab_center_y() - TAB_HEIGHT / 2.0
    } else {
        tab_center_y(empty_music) - TAB_HEIGHT / 2.0
    };
    let mut regions = vec![
        (Hit::Tab(Page::Music), [TAB_X, tab_y, TAB_WIDTH, TAB_HEIGHT]),
        (
            Hit::Tab(Page::Share),
            [TAB_X + TAB_WIDTH, tab_y, TAB_WIDTH, TAB_HEIGHT],
        ),
    ];
    if page == Page::Share {
        regions.extend([
            (
                Hit::Share(ShareAction::Device),
                [20.0, 34.0, width - 40.0, 30.0],
            ),
            (
                Hit::Share(ShareAction::Files),
                [20.0, 70.0, width - 40.0, 30.0],
            ),
            (
                Hit::Share(ShareAction::Clipboard),
                [20.0, 106.0, width - 40.0, 30.0],
            ),
        ]);
    }
    regions
}

pub fn hit_test(
    bounds: (f32, f32, f32, f32, f32),
    scale: f32,
    page: Page,
    empty_music: bool,
    x: f32,
    y: f32,
) -> Option<Hit> {
    let (left, top, width, _, _) = bounds;
    let (x, y) = ((x - left) / scale, (y - top) / scale);
    regions(width / scale, page, empty_music)
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
    empty_music: bool,
    hovered: Option<Hit>,
    state: &ShareState,
    color: crate::settings::IslandColor,
) -> Vec<LabelArea> {
    let (left, top, width, height, _) = bounds;
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    let mut labels = Vec::new();
    for (hit, [x, y, w, h]) in regions(width / scale, page, empty_music) {
        let hover = hovered == Some(hit);
        if let Hit::Tab(tab) = hit {
            let alpha = alpha * ((height / scale - y - h) / 8.0).clamp(0.0, 1.0);
            let ([r, g, b], opacity) = if tab == page {
                (color.light(), 52)
            } else if hover {
                (color.light(), 36)
            } else {
                (color.light(), 16)
            };
            paint.set_color_rgba8(r, g, b, (opacity as f32 * alpha) as u8);
            fill_tab(pixmap, &paint, left, top + y * scale, scale, tab);
            let [r, g, b] = color.light();
            paint.set_color_rgba8(r, g, b, (alpha * 255.0) as u8);
            let icon = match tab {
                Page::Music => NoticeIcon::Music,
                Page::Share => NoticeIcon::Share,
            };
            island::draw_notice_icon(
                pixmap,
                &paint,
                icon,
                left + (x + w / 2.0) * scale,
                top + (y + h / 2.0) * scale,
                scale * 0.85,
            );
            continue;
        }
        let ([r, g, b], opacity) = if hover {
            (color.light(), 40)
        } else if matches!(
            hit,
            Hit::Share(ShareAction::Device | ShareAction::Files | ShareAction::Clipboard)
        ) {
            (color.light(), 26)
        } else {
            (color.light(), 16)
        };
        paint.set_color_rgba8(r, g, b, (opacity as f32 * alpha) as u8);
        island::fill_rounded_rect(
            pixmap,
            &paint,
            left + x * scale,
            top + y * scale,
            w * scale,
            h * scale,
            10.0 * scale,
        );
        let (label, icon) = match hit {
            Hit::Share(ShareAction::Device) => {
                let label = if state.device_name.is_empty() {
                    "Vincular un dispositivo…".to_string()
                } else {
                    format!(
                        "Enviar a: {}{} · {}",
                        state.device_name,
                        if state.devices > 1 {
                            format!(" · {} equipos", state.devices)
                        } else {
                            String::new()
                        },
                        if state.device_available {
                            "Disponible"
                        } else {
                            "Sin conexión"
                        }
                    )
                };
                (label, Some(NoticeIcon::Wireless))
            }
            Hit::Share(ShareAction::Files) => {
                ("Elegir archivos…".to_string(), Some(NoticeIcon::Usb))
            }
            Hit::Share(ShareAction::Clipboard) => (
                "Enviar portapapeles".to_string(),
                Some(NoticeIcon::Keyboard),
            ),
            _ => continue,
        };
        if let Some(icon) = icon {
            let [r, g, b] = color.light();
            paint.set_color_rgba8(r, g, b, (alpha * 255.0) as u8);
            island::draw_notice_icon(
                pixmap,
                &paint,
                icon,
                left + (x + 18.0) * scale,
                top + (y + h / 2.0) * scale,
                scale * 0.7,
            );
        }
        let inset = if icon.is_some() { 36.0 } else { 14.0 };
        let text = label;
        labels.push(LabelArea {
            text,
            x: left + (x + inset) * scale,
            y: top + (y + (h - 18.0) / 2.0) * scale,
            w: (w - inset - 10.0) * scale,
            h: 18.0 * scale,
            alpha,
            strong: false,
            scroll: false,
        });
    }
    if page == Page::Share {
        let status = if !state.status.is_empty() {
            &state.status
        } else if state.busy {
            "Solicitud en curso…"
        } else if state.receiving {
            "~~~"
        } else {
            "Recepción pausada"
        };
        labels.push(LabelArea {
            text: status.to_string(),
            x: left + 20.0 * scale,
            y: top + 8.0 * scale,
            w: width - 92.0 * scale,
            h: 16.0 * scale,
            alpha: alpha * 0.7,
            strong: false,
            scroll: true,
        });
    }
    labels
}

pub fn draw_volume(
    pixmap: &mut Pixmap,
    bounds: (f32, f32, f32, f32, f32),
    scale: f32,
    alpha: f32,
    level: f32,
    empty_music: bool,
    color: crate::settings::IslandColor,
) {
    let (left, top, _, _, _) = bounds;
    let x = left + (TAB_X + 2.0 * TAB_WIDTH + 12.0) * scale;
    let y = top + (if empty_music { 44.0 } else { 64.0 }) * scale;
    let width = 96.0 * scale;
    let height = 4.0 * scale;
    let level = level.clamp(0.0, 1.0);
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    paint.set_color_rgba8(255, 255, 255, (alpha * 55.0) as u8);
    island::fill_rounded_rect(pixmap, &paint, x, y, width, height, height / 2.0);
    let [r, g, b] = color.light();
    paint.set_color_rgba8(r, g, b, (alpha * 255.0) as u8);
    island::fill_rounded_rect(pixmap, &paint, x, y, width * level, height, height / 2.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn las_pestañas_cubren_la_union_en_ambos_bordes() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let bounds = (3.3, 4.2, 284.0 * scale, SHARE_HEIGHT * scale, 24.0 * scale);
            let seam = (bounds.0 + (TAB_X + TAB_WIDTH) * scale).round() as u32;
            for (page, other, expected) in [
                (Page::Music, Page::Share, [52, 36]),
                (Page::Share, Page::Music, [36, 52]),
            ] {
                let tab_y = if page == Page::Share {
                    SHARE_TAB_Y
                } else {
                    TAB_Y
                };
                let top = (bounds.1 + tab_y * scale).round() as u32;
                let bottom = (bounds.1 + (tab_y + TAB_HEIGHT) * scale).round() as u32 - 1;
                let mut image = Pixmap::new(600, 400).unwrap();
                draw(
                    &mut image,
                    bounds,
                    scale,
                    1.0,
                    page,
                    false,
                    Some(Hit::Tab(other)),
                    &ShareState::default(),
                    crate::settings::IslandColor::default(),
                );
                for y in [top, bottom] {
                    assert_eq!(image.pixel(seam - 1, y).unwrap().alpha(), expected[0]);
                    assert_eq!(image.pixel(seam, y).unwrap().alpha(), expected[1]);
                }
            }
        }
    }
    #[test]
    fn pestañas_y_acciones_coinciden_a_distintos_dpi() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let width = island::EXPANDED_COMPACT.0;
            let bounds = (80.0, 8.0, width * scale, SHARE_HEIGHT * scale, 24.0 * scale);
            let regions = regions(width, Page::Share, false);
            for (index, (_, [x, y, w, h])) in regions.iter().enumerate() {
                assert!(*x >= 16.0 && x + w <= width - 16.0);
                assert!(*y >= 10.0 && y + h <= SHARE_HEIGHT - 12.0);
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
                        false,
                        bounds.0 + (x + w / 2.0) * scale,
                        bounds.1 + (y + h / 2.0) * scale
                    ) == Some(hit)
                );
            }
            assert!(hit_test(
                bounds,
                scale,
                Page::Music,
                false,
                bounds.0 + 60.0 * scale,
                bounds.1 + 100.0 * scale
            )
            .is_none());
            let mut image = Pixmap::new(640, 400).unwrap();
            draw_volume(
                &mut image,
                bounds,
                scale,
                1.0,
                0.68,
                false,
                crate::settings::IslandColor::default(),
            );
            let y = (bounds.1 + 66.0 * scale) as u32;
            for (x, expected) in [(94.0, 0), (110.0, 255), (185.0, 55), (200.0, 0)] {
                let x = (bounds.0 + x * scale) as u32;
                assert_eq!(image.pixel(x, y).unwrap().alpha(), expected);
            }
        }
    }
}
