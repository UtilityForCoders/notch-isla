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

const TAB_X: f32 = 16.0;
const TAB_Y: f32 = 12.0;
const TAB_WIDTH: f32 = 36.0;
const TAB_HEIGHT: f32 = 28.0;

fn fill_tab(pixmap: &mut Pixmap, paint: &Paint, left: f32, top: f32, scale: f32, tab: Page) {
    let x = left + TAB_X * scale;
    let seam = (x + TAB_WIDTH * scale).round();
    let y = (top + TAB_Y * scale).round();
    let bottom = (top + (TAB_Y + TAB_HEIGHT) * scale).round();
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

fn regions(width: f32, page: Page) -> Vec<(Hit, [f32; 4])> {
    let mut regions = vec![
        (Hit::Tab(Page::Music), [TAB_X, TAB_Y, TAB_WIDTH, TAB_HEIGHT]),
        (
            Hit::Tab(Page::Share),
            [TAB_X + TAB_WIDTH, TAB_Y, TAB_WIDTH, TAB_HEIGHT],
        ),
    ];
    if page == Page::Share {
        regions.extend([
            (
                Hit::Share(ShareAction::Files),
                [20.0, 46.0, width - 40.0, 32.0],
            ),
            (
                Hit::Share(ShareAction::Clipboard),
                [20.0, 84.0, width - 40.0, 32.0],
            ),
            (Hit::Share(ShareAction::Pair), [20.0, 122.0, 88.0, 28.0]),
            (
                Hit::Share(ShareAction::Devices),
                [114.0, 122.0, width - 176.0, 28.0],
            ),
            (
                Hit::Share(ShareAction::Receive),
                [width - 56.0, 122.0, 36.0, 28.0],
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
    color: crate::settings::IslandColor,
) -> Vec<LabelArea> {
    let (left, top, width, _, _) = bounds;
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    let mut labels = Vec::new();
    for (hit, [x, y, w, h]) in regions(width / scale, page) {
        let hover = hovered == Some(hit);
        if let Hit::Tab(tab) = hit {
            let ([r, g, b], opacity) = if tab == page {
                (color.blend(35, 48), 185)
            } else if hover {
                (color.blend(42, 25), 220)
            } else {
                (color.blend(23, 17), 150)
            };
            paint.set_color_rgba8(r, g, b, (opacity as f32 * alpha) as u8);
            fill_tab(pixmap, &paint, left, top, scale, tab);
            let [r, g, b] = color.blend(255, 42);
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
            (color.blend(42, 25), 220)
        } else if matches!(hit, Hit::Share(ShareAction::Files | ShareAction::Clipboard)) {
            (color.blend(25, 20), 220)
        } else {
            (color.blend(24, 17), 150)
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
            let [r, g, b] = color.blend(255, 42);
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
            "~~~"
        } else {
            "Recepción pausada"
        };
        labels.push(LabelArea {
            text: status.to_string(),
            x: left + 20.0 * scale,
            y: top + 154.0 * scale,
            w: width - 40.0 * scale,
            h: 16.0 * scale,
            alpha: alpha * 0.7,
            strong: false,
            scroll: true,
        });
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn las_pestañas_cubren_la_union_en_ambos_bordes() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let bounds = (3.3, 4.2, 284.0 * scale, 176.0 * scale, 24.0 * scale);
            let seam = (bounds.0 + (TAB_X + TAB_WIDTH) * scale).round() as u32;
            let top = (bounds.1 + TAB_Y * scale).round() as u32;
            let bottom = (bounds.1 + (TAB_Y + TAB_HEIGHT) * scale).round() as u32 - 1;
            for (page, other, expected) in [
                (Page::Music, Page::Share, [185, 220]),
                (Page::Share, Page::Music, [220, 185]),
            ] {
                let mut image = Pixmap::new(600, 360).unwrap();
                draw(
                    &mut image,
                    bounds,
                    scale,
                    1.0,
                    page,
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
            let bounds = (
                80.0,
                8.0,
                284.0 * scale,
                island::EXPANDED_COMPACT.1 * scale,
                24.0 * scale,
            );
            let regions = regions(284.0, Page::Share);
            for (index, (_, [x, y, w, h])) in regions.iter().enumerate() {
                assert!(*x >= 16.0 && x + w <= 284.0 - 16.0);
                assert!(*y >= 12.0 && y + h <= island::EXPANDED_COMPACT.1 - 16.0);
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
            assert!(hit_test(
                bounds,
                scale,
                Page::Music,
                bounds.0 + 60.0 * scale,
                bounds.1 + 100.0 * scale
            )
            .is_none());
        }
    }
}
