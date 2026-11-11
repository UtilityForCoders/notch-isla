use tiny_skia::{ColorU8, Paint, Pixmap, PixmapPaint, Transform};

use crate::island::{fill_rounded_rect, LabelArea};
use crate::settings::IslandColor;

const LEFT: f32 = 206.0;
const TOP: f32 = 51.0;
const WIDTH: f32 = 308.0;
const HEIGHT: f32 = 446.0;
const CX: f32 = LEFT + WIDTH / 2.0;
const CY: f32 = TOP + 165.0;
const RADIUS: f32 = 112.0;
const TRACK_X: f32 = LEFT + 24.0;
const TRACK_W: f32 = WIDTH - 48.0;
const TRACK_Y: f32 = TOP + 298.0;
const HEX_Y: f32 = TOP + 337.0;
const BUTTON_Y: f32 = TOP + 387.0;

#[derive(Clone, Copy)]
enum Drag {
    Wheel,
    Brightness,
}

pub enum Action {
    Continue,
    Cancel,
    Apply(IslandColor),
}

pub struct Picker {
    hue: f32,
    saturation: f32,
    brightness: f32,
    hex: String,
    editing: bool,
    replace_hex: bool,
    drag: Option<Drag>,
    wheel: Option<Pixmap>,
}

fn rgb(hue: f32, saturation: f32, brightness: f32) -> IslandColor {
    let sector = hue.rem_euclid(1.0) * 6.0;
    let chroma = saturation * brightness;
    let x = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let min = brightness - chroma;
    let channels = match sector as u8 {
        0 => [chroma, x, 0.0],
        1 => [x, chroma, 0.0],
        2 => [0.0, chroma, x],
        3 => [0.0, x, chroma],
        4 => [x, 0.0, chroma],
        _ => [chroma, 0.0, x],
    };
    IslandColor(channels.map(|channel| ((channel + min) * 255.0).round() as u8))
}

fn hsv(color: IslandColor) -> (f32, f32, f32) {
    let [r, g, b] = color.0.map(|channel| channel as f32 / 255.0);
    let max = r.max(g).max(b);
    let delta = max - r.min(g).min(b);
    let hue = if delta == 0.0 {
        0.0
    } else if max == r {
        (g - b) / delta
    } else if max == g {
        2.0 + (b - r) / delta
    } else {
        4.0 + (r - g) / delta
    };
    (
        hue.rem_euclid(6.0) / 6.0,
        if max > 0.0 { delta / max } else { 0.0 },
        max,
    )
}

fn wheel_position(dx: f32, dy: f32, radius: f32) -> (f32, f32) {
    let hue = ((-dy).atan2(dx) / std::f32::consts::TAU).rem_euclid(1.0);
    (hue, (dx.hypot(dy) / radius).min(1.0))
}

impl Picker {
    pub fn new(color: IslandColor) -> Self {
        let (hue, saturation, brightness) = hsv(color);
        Self {
            hue,
            saturation,
            brightness,
            hex: format!("{:06X}", color.rgb()),
            editing: false,
            replace_hex: false,
            drag: None,
            wheel: None,
        }
    }

    fn color(&self) -> IslandColor {
        rgb(self.hue, self.saturation, self.brightness)
    }

    fn apply(&self) -> Action {
        if self.hex.len() == 6 {
            Action::Apply(self.color())
        } else {
            Action::Continue
        }
    }

    pub fn press(&mut self, x: f32, y: f32) -> Action {
        if (x - CX).hypot(y - CY) <= RADIUS {
            self.drag = Some(Drag::Wheel);
        } else if (TRACK_X..=TRACK_X + TRACK_W).contains(&x)
            && (TRACK_Y - 8.0..=TRACK_Y + 19.0).contains(&y)
        {
            self.drag = Some(Drag::Brightness);
        } else if (TRACK_X..TRACK_X + TRACK_W - 52.0).contains(&x)
            && (HEX_Y..HEX_Y + 34.0).contains(&y)
        {
            self.editing = true;
            self.replace_hex = true;
        } else if (BUTTON_Y..BUTTON_Y + 34.0).contains(&y) {
            if (CX + 12.0..=LEFT + WIDTH - 24.0).contains(&x) {
                return self.apply();
            }
            if (TRACK_X..=CX - 12.0).contains(&x) {
                return Action::Cancel;
            }
        }
        self.drag_to(x, y);
        Action::Continue
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn release(&mut self) {
        self.drag = None;
    }

    pub fn drag_to(&mut self, x: f32, y: f32) {
        match self.drag {
            Some(Drag::Wheel) => {
                (self.hue, self.saturation) = wheel_position(x - CX, y - CY, RADIUS)
            }
            Some(Drag::Brightness) => self.brightness = ((x - TRACK_X) / TRACK_W).clamp(0.0, 1.0),
            None => return,
        }
        self.editing = false;
        self.hex = format!("{:06X}", self.color().rgb());
    }

    pub fn key(&mut self, key: u32) -> Action {
        match key {
            0x1B => Action::Cancel,
            0x0D => self.apply(),
            0x09 => {
                self.editing = true;
                self.replace_hex = true;
                Action::Continue
            }
            _ => Action::Continue,
        }
    }

    pub fn character(&mut self, c: char) {
        if !self.editing || !(c.is_ascii_hexdigit() || c == '\u{8}') {
            return;
        }
        if self.replace_hex {
            self.hex.clear();
            self.replace_hex = false;
        } else if c == '\u{8}' {
            self.hex.pop();
        }
        if c.is_ascii_hexdigit() && self.hex.len() < 6 {
            self.hex.push(c.to_ascii_uppercase());
        }
        if self.hex.len() == 6 {
            let hex = self.hex.clone();
            self.set_hex(&hex);
        }
    }

    pub fn paste_hex(&mut self, text: &str) {
        if self.editing {
            self.set_hex(text.trim().trim_start_matches('#'));
        }
    }

    fn set_hex(&mut self, hex: &str) {
        if hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            let [_, r, g, b] = u32::from_str_radix(hex, 16).unwrap().to_be_bytes();
            (self.hue, self.saturation, self.brightness) = hsv(IslandColor([r, g, b]));
            self.hex = hex.to_ascii_uppercase();
            self.replace_hex = false;
        }
    }

    pub fn draw(&mut self, pixmap: &mut Pixmap, labels: &mut Vec<LabelArea>, scale: f32) {
        labels.retain(|label| {
            label.x + label.w <= LEFT * scale
                || label.x >= (LEFT + WIDTH) * scale
                || label.y + label.h <= TOP * scale
                || label.y >= (TOP + HEIGHT) * scale
        });
        for label in &mut *labels {
            label.alpha *= 0.35;
        }
        let mut paint = Paint {
            anti_alias: true,
            ..Default::default()
        };
        macro_rules! rect {
            ($x:expr, $y:expr, $w:expr, $h:expr, $radius:expr, $rgba:expr) => {{
                let [r, g, b, a] = $rgba;
                paint.set_color_rgba8(r, g, b, a);
                fill_rounded_rect(
                    pixmap,
                    &paint,
                    $x * scale,
                    $y * scale,
                    $w * scale,
                    $h * scale,
                    $radius * scale,
                );
            }};
        }
        let (panel_width, panel_height) = (
            pixmap.width() as f32 / scale,
            pixmap.height() as f32 / scale,
        );
        rect!(0.0, 0.0, panel_width, panel_height, 18.0, [0, 0, 0, 145]);
        rect!(
            LEFT - 1.0,
            TOP - 1.0,
            WIDTH + 2.0,
            HEIGHT + 2.0,
            15.0,
            [65, 65, 72, 255]
        );
        rect!(LEFT, TOP, WIDTH, HEIGHT, 14.0, [27, 27, 31, 255]);
        let diameter = (RADIUS * 2.0 * scale).round().max(1.0) as u32;
        if self
            .wheel
            .as_ref()
            .is_none_or(|wheel| wheel.width() != diameter)
        {
            self.wheel = Pixmap::new(diameter, diameter).map(|mut wheel| {
                let radius = diameter as f32 / 2.0;
                for (index, pixel) in wheel.pixels_mut().iter_mut().enumerate() {
                    let x = (index as u32 % diameter) as f32 + 0.5 - radius;
                    let y = (index as u32 / diameter) as f32 + 0.5 - radius;
                    let alpha = ((radius - x.hypot(y) + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
                    if alpha != 0 {
                        let (hue, saturation) = wheel_position(x, y, radius);
                        let [r, g, b] = rgb(hue, saturation, 1.0).0;
                        *pixel = ColorU8::from_rgba(r, g, b, alpha).premultiply();
                    }
                }
                wheel
            });
        }
        if let Some(wheel) = &self.wheel {
            pixmap.draw_pixmap(
                ((CX - RADIUS) * scale).round() as i32,
                ((CY - RADIUS) * scale).round() as i32,
                wheel.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
        let angle = self.hue * std::f32::consts::TAU;
        let mx = CX + angle.cos() * self.saturation * RADIUS;
        let my = CY - angle.sin() * self.saturation * RADIUS;
        let [r, g, b] = rgb(self.hue, self.saturation, 1.0).0;
        rect!(mx - 7.0, my - 7.0, 14.0, 14.0, 7.0, [18, 18, 20, 255]);
        rect!(mx - 5.0, my - 5.0, 10.0, 10.0, 5.0, [255, 255, 255, 255]);
        rect!(mx - 3.0, my - 3.0, 6.0, 6.0, 3.0, [r, g, b, 255]);
        // El brillo recorre negro a la intensidad máxima del tono elegido.
        for step in 0..100 {
            let [r, g, b] = rgb(self.hue, self.saturation, step as f32 / 99.0).0;
            let width = TRACK_W / 100.0;
            rect!(
                TRACK_X + step as f32 * width,
                TRACK_Y,
                width + 0.6,
                10.0,
                0.0,
                [r, g, b, 255]
            );
        }
        let marker = TRACK_X + self.brightness * TRACK_W;
        rect!(
            marker - 3.0,
            TRACK_Y - 3.0,
            6.0,
            16.0,
            3.0,
            [240, 240, 244, 255]
        );
        let field = if self.editing {
            [70, 70, 82, 255]
        } else {
            [42, 42, 48, 255]
        };
        rect!(TRACK_X, HEX_Y, TRACK_W - 52.0, 34.0, 7.0, field);
        let [r, g, b] = self.color().0;
        rect!(
            TRACK_X + TRACK_W - 40.0,
            HEX_Y,
            40.0,
            34.0,
            7.0,
            [r, g, b, 255]
        );
        rect!(TRACK_X, BUTTON_Y, 118.0, 34.0, 8.0, [47, 47, 53, 255]);
        rect!(CX + 12.0, BUTTON_Y, 118.0, 34.0, 8.0, [68, 68, 83, 255]);
        for (text, x, y, width, strong) in [
            ("Color del notch".into(), TRACK_X, TOP + 15.0, TRACK_W, true),
            ("Brillo".into(), TRACK_X, TRACK_Y - 24.0, 80.0, false),
            (
                format!("#{}", self.hex),
                TRACK_X + 12.0,
                HEX_Y + 7.0,
                170.0,
                true,
            ),
            (
                "Cancelar".into(),
                TRACK_X + 28.0,
                BUTTON_Y + 7.0,
                90.0,
                false,
            ),
            ("Aplicar".into(), CX + 44.0, BUTTON_Y + 7.0, 80.0, true),
        ] {
            labels.push(LabelArea {
                text,
                x: x * scale,
                y: y * scale,
                w: width * scale,
                h: 20.0 * scale,
                alpha: 1.0,
                strong,
                scroll: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conserva_el_rgb_al_abrir_y_al_introducir_hexadecimal() {
        for color in [[0, 0, 0], [255, 255, 255], [1, 117, 239], [255, 46, 140]] {
            let mut picker = Picker::new(IslandColor(color));
            assert!(matches!(picker.key(0x0D), Action::Apply(value) if value.0 == color));
            picker.press(TRACK_X + 5.0, HEX_Y + 5.0);
            picker.character('A');
            assert!(matches!(picker.key(0x0D), Action::Continue));
            picker.paste_hex("#ff2e8c");
            assert!(matches!(picker.key(0x0D), Action::Apply(value) if value.0 == [255, 46, 140]));
            assert!(matches!(picker.key(0x1B), Action::Cancel));
        }
    }
}
