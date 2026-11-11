//! Geometría y dibujo de la isla flotante. Todo en píxeles lógicos; `scale` convierte según el DPI.
//!
//! La isla es un cuadro flotante con las cuatro esquinas redondeadas, separada del borde superior
//! de la pantalla por `TOP_GAP` (sin tocar el techo).
//! En reposo queda una línea superior; al abrirse muestra directamente el reproductor.
//! Expandida muestra una tarjeta completa flotante: reproductor con tiempos y controles a la izquierda,
//! y letra sincronizada a la derecha.

use tiny_skia::{
    FillRule, FilterQuality, Paint, Path, PathBuilder, Pattern, Pixmap, SpreadMode, Transform,
};

/// (ancho, alto, radio de las esquinas)
/// Línea desde la que empieza la animación de apertura.
const RESTING: (f32, f32, f32) = (88.0, 3.0, 1.5);

/// Expandida compacta (sin letra): tarjeta flotante con reproductor.
pub const EXPANDED_COMPACT: (f32, f32, f32) = (264.0, 176.0, 24.0);

/// Expandida amplia (con letra sincronizada): tarjeta completa con reproductor y letra.
pub const EXPANDED_LYRICS: (f32, f32, f32) = (500.0, EXPANDED_COMPACT.1, 24.0);

/// Con un aviso del sistema (salida de audio, temporizador, atajo, actualización): una sola
/// fila con el ícono y los textos, sin el espacio del reproductor.
const NOTICE: (f32, f32, f32) = (252.0, 68.0, 22.0);
/// Recuadro del ícono del aviso: (margen, lado, radio).
const NOTICE_ICON: (f32, f32, f32) = (9.0, 38.0, 9.0);

/// Separación lógica entre el borde superior de la pantalla y la isla flotante (sin tocar el techo).
const TOP_GAP: f32 = 8.0;
pub const HEADER_HEIGHT: f32 = 44.0;
pub const EXPANDED_CONTENT_START: f32 = 0.45;

/// Medidas del panel izquierdo de reproducción (píxeles lógicos).
const COVER_PAD_X: f32 = 20.0;
/// Margen superior lógico de la carátula dentro del reproductor.
const COVER_PAD_TOP: f32 = 4.0;
/// Lado lógico de la carátula expandida.
const COVER_SIZE: f32 = 48.0;
const PROGRESS_Y: f32 = HEADER_HEIGHT + COVER_PAD_TOP + COVER_SIZE + 18.0;
const CONTROLS_Y: f32 = PROGRESS_Y + 30.0;
/// Radio lógico de las esquinas de la carátula.
const COVER_RADIUS: f32 = 10.0;
/// Ancho lógico reservado al reproductor, independiente del panel de letra.
const PLAYER_W: f32 = EXPANDED_COMPACT.0 - 2.0 * COVER_PAD_X;

/// Barra de progreso: grosor (normal y con mouse encima) y zona de clic.
const PROGRESS_THICK: (f32, f32) = (3.2, 5.0);
/// Media altura lógica de la zona de clic, más generosa que la barra dibujada.
const PROGRESS_HIT: f32 = 8.0;

/// Margen para que el rebote del resorte no se recorte en el borde de la ventana.
const OVERSHOOT: f32 = 1.08;

/// Radio lógico de la zona de clic (y del resaltado) de los botones.
const BUTTON_RADIUS: f32 = 15.0;

/// Tamaño lógico de la ventana: cabe el cuadro expandido más la separación superior y el rebote.
pub fn window_size() -> (f32, f32) {
    (
        EXPANDED_LYRICS.0 * OVERSHOOT + 16.0,
        TOP_GAP + EXPANDED_LYRICS.1 * OVERSHOOT + 16.0,
    )
}

/// Alto de la línea de tiempo que queda pegada al borde superior cuando la isla se oculta sola.
const EDGE_STRIP_H: f32 = 3.0;

/// Ancho en píxeles físicos de la línea del borde superior.
pub fn edge_strip_width(scale: f32) -> f32 {
    RESTING.0 * scale
}

/// Línea de tiempo en el borde superior: indica que la isla sigue disponible.
/// Tocar la línea con el mouse la hace volver; todo su ancho recibe el mouse.
/// `progress`: avance si la app lo informa; `update`: marca una versión nueva con un segmento azul.
pub fn draw_edge_strip(
    pixmap: &mut Pixmap,
    scale: f32,
    progress: Option<f32>,
    accent: [u8; 3],
    update: bool,
) {
    let w = edge_strip_width(scale);
    let x = (pixmap.width() as f32 - w) / 2.0;
    let h = (EDGE_STRIP_H * scale).round().max(2.0);
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    // Pista semitransparente: se distingue sobre fondos claros y oscuros.
    paint.set_color_rgba8(0, 0, 0, 170);
    fill_rounded_rect(pixmap, &paint, x, 0.0, w, h, h / 2.0);
    let [r, g, b] = accent;
    match progress {
        Some(fraction) => {
            let filled = w * fraction.clamp(0.0, 1.0);
            if filled >= 1.0 {
                paint.set_color_rgba8(r, g, b, 255);
                fill_rounded_rect(pixmap, &paint, x, 0.0, filled, h, h / 2.0);
            }
        }
        None => {
            // Sin tiempo informado: toda la línea con el color, más tenue.
            paint.set_color_rgba8(r, g, b, 140);
            fill_rounded_rect(pixmap, &paint, x, 0.0, w, h, h / 2.0);
        }
    }
    if update {
        let [r, g, b] = UPDATE_COLOR;
        paint.set_color_rgba8(r, g, b, 255);
        fill_rounded_rect(
            pixmap,
            &paint,
            x + w - 8.0 * scale,
            0.0,
            7.0 * scale,
            h,
            h / 2.0,
        );
    }
}

/// Botones del reproductor, incluido el interruptor de letra sincronizada.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Button {
    Lyrics,
    Prev,
    PlayPause,
    Next,
}

/// Lo que hay bajo el mouse en la isla expandida.
#[derive(Clone, Copy, PartialEq)]
pub enum Hit {
    Tab(crate::workspace::Page),
    Share(crate::workspace::ShareAction),
    Button(Button),
    /// Sobre la barra de progreso, en esa fracción (0 a 1) de la canción.
    Seek(f32),
    /// Sobre la carátula, con varias apps sonando: un clic pasa a la siguiente.
    Cover,
    /// Sobre el botón de actualización disponible: un clic la instala.
    Update,
    Battery,
}

const UPDATE_COLOR: [u8; 3] = [75, 166, 255];
const BATTERY_COLOR: [u8; 3] = [255, 174, 66];
const INDICATOR_HIT_R: f32 = 8.0;

#[derive(Clone, Copy, Default)]
pub struct Indicators {
    pub battery_low: bool,
    pub update: bool,
    pub hovered: Option<Hit>,
}

fn indicator_center(g: &Geometry, p: f32, battery: bool) -> (f32, f32) {
    let p = p.clamp(0.0, 1.0);
    let s = g.scale;
    let x = if battery {
        g.x0 + 14.0 * s * (1.0 - p) + (g.w - 50.0 * s) * p
    } else {
        g.x1() - (14.0 + 10.0 * p) * s
    };
    (x, g.y0 + ((17.0 + 9.0 * p) * s).min(g.h / 2.0))
}

#[allow(clippy::too_many_arguments)]
pub fn indicator_hit_test(
    max_w: f32,
    max_h: f32,
    scale: f32,
    p: f32,
    presence: f32,
    shape: (f32, f32),
    indicators: Indicators,
    x: f32,
    y: f32,
) -> Option<Hit> {
    let g = geometry(max_w, max_h, scale, p, presence, shape);
    if presence < 0.9 || g.alpha < 1.0 {
        return None;
    }
    for (shown, hit, battery) in [
        (indicators.battery_low, Hit::Battery, true),
        (indicators.update, Hit::Update, false),
    ] {
        if !shown {
            continue;
        }
        let (cx, cy) = indicator_center(&g, p, battery);
        if (x - cx).abs() <= INDICATOR_HIT_R * scale && (y - cy).abs() <= INDICATOR_HIT_R * scale {
            return Some(hit);
        }
    }
    None
}

fn draw_indicators(pixmap: &mut Pixmap, g: &Geometry, p: f32, indicators: Indicators) {
    if g.alpha <= 0.0 {
        return;
    }
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    for (shown, hit, battery, icon, color) in [
        (
            indicators.battery_low,
            Hit::Battery,
            true,
            NoticeIcon::Battery,
            BATTERY_COLOR,
        ),
        (
            indicators.update,
            Hit::Update,
            false,
            NoticeIcon::Installing,
            UPDATE_COLOR,
        ),
    ] {
        if !shown {
            continue;
        }
        let (cx, cy) = indicator_center(g, p, battery);
        let s = g.scale;
        let [r, green, b] = color;
        paint.set_color_rgba8(
            r,
            green,
            b,
            if indicators.hovered == Some(hit) {
                65
            } else {
                28
            },
        );
        fill_rounded_rect(
            pixmap,
            &paint,
            cx - 8.0 * s,
            cy - 9.0 * s,
            16.0 * s,
            18.0 * s,
            5.0 * s,
        );
        paint.set_color_rgba8(r, green, b, 245);
        draw_notice_icon(pixmap, &paint, icon, cx, cy, 0.52 * s);
    }
}

/// Marca naranja integrada en el extremo izquierdo de la barrita.
pub fn draw_edge_battery_marker(pixmap: &mut Pixmap, scale: f32) {
    let x = (pixmap.width() as f32 - edge_strip_width(scale)) / 2.0;
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    let [r, g, b] = BATTERY_COLOR;
    paint.set_color_rgba8(r, g, b, 245);
    let h = (EDGE_STRIP_H * scale).round().max(2.0);
    fill_rounded_rect(pixmap, &paint, x + scale, 0.0, 7.0 * scale, h, h / 2.0);
}

/// Estado de la reproducción; `None` en `draw` = no hay música (sin botones ni indicador).
pub struct Controls {
    pub playing: bool,
    pub hovered: Option<Button>,
    /// El mouse está sobre la barra de progreso (se engrosa): en qué fracción y a qué tiempo
    /// saltaría con un clic, ya formateado.
    pub seek_hover: Option<(f32, String)>,
    /// Color de las barras de audio y del avance en el borde (tomado de la carátula).
    pub accent: [u8; 3],
    /// Fracción reproducida, o `None` si la app no informa el avance (no se dibuja la barra).
    pub progress: Option<f32>,
    /// (posición de la app mostrada, cantidad de apps con música): con 2 o más se dibujan
    /// puntos bajo la carátula.
    pub sessions: (usize, usize),
    /// El mouse está sobre la carátula y hay otra app a la que cambiar (se aclara).
    pub cover_hovered: bool,
    /// Botón recién pulsado y avance de su animación (0 a 1).
    pub pressed: Option<(Button, f32)>,
    /// Nivel real de cada barra (0 a 1, de graves a agudos) si se está escuchando el audio;
    /// `None` = la animación de siempre.
    pub levels: Option<[f32; 4]>,
    /// La letra fue activada por el usuario; resalta el botón correspondiente.
    pub lyrics_active: bool,
    /// Hay letra sincronizada confirmada para la canción actual.
    pub lyrics_available: bool,
}

/// Etiquetas de tiempo reproducido y restante, ya formateadas como minutos y segundos.
#[derive(Clone)]
pub struct TimeInfo {
    pub current: String,
    pub remaining: String,
}

/// Versos alrededor del actual para el carrusel, y mensaje de búsqueda o ausencia de letra.
#[derive(Clone)]
pub struct LyricsInfo {
    /// (distancia al verso que se canta, texto): -1 = anterior, 0 = actual, 1 = siguiente...
    /// Texto vacío = pausa sin letra (se dibuja una nota musical).
    pub lines: Vec<(i32, String)>,
    /// Cuánto le falta al carrusel para llegar a su sitio, en versos: positivo = va subiendo,
    /// negativo = va bajando, 0 = quieto.
    pub shift: f32,
    /// Hasta dónde va cantado el verso actual (unidades UTF-16 de su texto, con decimales);
    /// `None` antes del primer verso.
    pub sung: Option<f32>,
    pub status: Option<String>,
}

/// Zonas de texto en píxeles físicos que DirectWrite dibuja después del fondo.
pub struct LabelArea {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub alpha: f32,
    pub strong: bool,
    pub scroll: bool,
}

pub struct LayoutAreas {
    pub labels: Vec<LabelArea>,
    pub text: TextArea,
    pub time: Option<TimeArea>,
    pub lyrics: Option<LyricsArea>,
    pub seek: Option<SeekTip>,
}

/// Globo con el tiempo al que se saltaría, sobre el punto de la barra bajo el mouse.
pub struct SeekTip {
    /// Centro horizontal deseado y centro vertical de la barra.
    pub x: f32,
    pub bar_cy: f32,
    /// Límites horizontales: el globo no se sale de la isla.
    pub min_x: f32,
    pub max_x: f32,
    pub text: String,
    pub alpha: f32,
}

/// Posiciones y opacidad de los tiempos junto a la barra de progreso.
pub struct TimeArea {
    pub cur_x: f32,
    pub rem_x: f32,
    pub y: f32,
    pub current: String,
    pub remaining: String,
    pub alpha: f32,
}

/// Columna de letra sincronizada: límites, versos y opacidad de la transición.
pub struct LyricsArea {
    pub x: f32,
    pub w: f32,
    pub cy: f32,
    pub half_height: f32,
    /// Como en `LyricsInfo`.
    pub lines: Vec<(i32, String)>,
    pub shift: f32,
    pub sung: Option<f32>,
    pub status: Option<String>,
    pub alpha: f32,
}

/// Ícono de un aviso del sistema.
#[derive(Clone, Copy, PartialEq)]
pub enum NoticeIcon {
    Music,
    Share,
    Mouse,
    Gamepad,
    Speaker,
    Device,
    Bluetooth,
    Wireless,
    Usb,
    Battery,
    /// Salida de audio (audífonos, parlantes).
    Audio,
    Timer,
    /// Esperando la combinación del atajo nuevo.
    Keyboard,
    /// Actualización: instalándose, instalada o fallida.
    Installing,
    Done,
    Failed,
}

pub fn device_icon(kind: crate::battery::DeviceKind) -> NoticeIcon {
    use crate::battery::DeviceKind;
    match kind {
        DeviceKind::Mouse => NoticeIcon::Mouse,
        DeviceKind::Keyboard => NoticeIcon::Keyboard,
        DeviceKind::Headphones => NoticeIcon::Audio,
        DeviceKind::Speaker => NoticeIcon::Speaker,
        DeviceKind::Gamepad => NoticeIcon::Gamepad,
        DeviceKind::Unknown => NoticeIcon::Device,
    }
}

/// Aviso que ocupa la isla expandida: ícono a la izquierda y textos (los dibuja DirectWrite).
pub struct Notice {
    pub icon: NoticeIcon,
    pub color: [u8; 3],
}

/// Duración de la animación al pulsar un botón.
pub const PRESS_SECS: f32 = 0.35;

/// Tamaño del ícono pulsado: se encoge al 80 %, rebota un poco por encima y se asienta.
fn press_scale(t: f32) -> f32 {
    let u = t.clamp(0.0, 1.0);
    1.0 - 0.2 * (1.0 - u).powi(2) * (3.0 * std::f32::consts::PI * u).cos()
}

/// Dónde van título y artista (píxeles físicos): columna horizontal, centro vertical de la
/// fila y opacidad.
pub struct TextArea {
    pub x: f32,
    pub w: f32,
    pub cy: f32,
    pub alpha: f32,
}

/// Forma de la isla flotante para un progreso dado, en píxeles físicos.
struct Geometry {
    x0: f32,
    y0: f32,
    w: f32,
    h: f32,
    r: f32,
    /// Opacidad del contenido: aparece al final de la expansión.
    alpha: f32,
    scale: f32,
}

impl Geometry {
    /// Centro vertical de la isla.
    fn cy(&self) -> f32 {
        self.y0 + self.h / 2.0
    }

    fn x1(&self) -> f32 {
        self.x0 + self.w
    }

    /// Carátula de la isla expandida: (x, y, lado).
    fn cover_rect(&self) -> (f32, f32, f32) {
        let s = self.scale;
        (
            self.x0 + COVER_PAD_X * s,
            self.y0 + (COVER_PAD_TOP + HEADER_HEIGHT) * s,
            COVER_SIZE * s,
        )
    }

    /// Barra de progreso: (x, centro vertical, ancho).
    fn progress_bar(&self) -> (f32, f32, f32) {
        let s = self.scale;
        let px = self.x0 + COVER_PAD_X * s;
        let pw = PLAYER_W * s;
        let bar_x = px + 40.0 * s;
        let bar_w = pw - 82.0 * s;
        let bar_cy = self.y0 + PROGRESS_Y * s;
        (bar_x, bar_cy, bar_w.max(10.0 * s))
    }
}

/// `presence`: 1 = en su sitio flotando a `TOP_GAP` del borde superior; 0 = escondida por encima del borde.
/// `lyrics_p`: 1 = ancho completo con letra sincronizada; 0 = ancho compacto sólo reproductor.
/// `notice_p`: 1 = forma de aviso (`NOTICE`), que reemplaza a la de la música mientras dura.
/// Al aparecer baja desde arriba con rebote del resorte y queda flotando sin tocar el techo.
fn geometry(
    max_w: f32,
    max_h: f32,
    scale: f32,
    p: f32,
    presence: f32,
    (lyrics_p, notice_p): (f32, f32),
) -> Geometry {
    let p = p.clamp(-0.1, 1.1);
    let lerp = |a: f32, b: f32| a + (b - a) * p;
    let (expanded_w, expanded_h) = expanded_size((lyrics_p, notice_p));
    let w = (lerp(RESTING.0, expanded_w) * scale).clamp(1.0, max_w);
    let h = lerp(RESTING.1, expanded_h) * scale;
    let gap = TOP_GAP * scale * p.clamp(0.0, 1.0);

    // Lo que queda por encima de y = 0 no se dibuja: así entra y sale por el borde superior.
    // Al posarse (presence = 1.0), y0 queda exactamente en `gap` (separada del techo, flotando).
    let y0 = gap - (1.0 - presence.clamp(-0.1, 1.1)) * (h + gap);
    let h = h.clamp(1.0, (max_h - y0).max(1.0));

    Geometry {
        x0: (max_w - w) / 2.0,
        y0,
        w,
        h,
        r: (lerp(RESTING.2, EXPANDED_LYRICS.2) * scale).clamp(0.0, (w / 2.0).min(h / 2.0)),
        alpha: ((p - EXPANDED_CONTENT_START) / (1.0 - EXPANDED_CONTENT_START)).clamp(0.0, 1.0),
        scale,
    }
}

/// Contorno exacto para recortar el fondo compuesto, sin incluir controles ni márgenes.
pub fn backdrop_geometry(
    max_w: f32,
    max_h: f32,
    scale: f32,
    p: f32,
    presence: f32,
    shape: (f32, f32),
) -> (f32, f32, f32, f32, f32) {
    let g = geometry(max_w, max_h, scale, p, presence, shape);
    (g.x0, g.y0, g.w, g.h, g.r)
}

/// Tamaño lógico de la isla expandida (ancho, alto) para `shape` = (`lyrics_p`, `notice_p`).
fn expanded_size((lyrics_p, notice_p): (f32, f32)) -> (f32, f32) {
    let lyrics_p = lyrics_p.clamp(-0.1, 1.15);
    let n = notice_p.clamp(-0.1, 1.1);
    let toward_notice = |music: f32, notice: f32| music + (notice - music) * n;
    let music_w = EXPANDED_COMPACT.0 + (EXPANDED_LYRICS.0 - EXPANDED_COMPACT.0) * lyrics_p;
    (
        toward_notice(music_w, NOTICE.0),
        toward_notice(EXPANDED_LYRICS.1, NOTICE.1),
    )
}

/// Rectángulo (x0, y0, x1, y1, píxeles físicos) donde `draw` puede pintar algo en este cuadro:
/// la isla con un margen para lo que asoma de ella (el recorte de
/// la letra). Fuera de él la ventana queda transparente, así que basta con limpiar, copiar y
/// mandar a Windows esta zona: en reposo es una parte mínima de la ventana.
pub fn bounds(
    max_w: f32,
    max_h: f32,
    scale: f32,
    p: f32,
    presence: f32,
    shape: (f32, f32),
) -> (f32, f32, f32, f32) {
    let g = geometry(max_w, max_h, scale, p, presence, shape);
    let margin = 8.0 * scale;
    let (mut x0, mut x1, mut y1) = (g.x0, g.x1(), g.y0 + g.h);
    // Mientras se expande o se encoge, el contenido (botones, tiempos, letra) se ubica como
    // en la isla expandida y se desvanece: asoma fuera del cuadro negro, que es más chico.
    // La zona lo cubre entero, si no quedarían restos de esos cuadros al encogerse.
    if g.alpha > 0.0 {
        let (w, mut h) = expanded_size(shape);
        if shape.1 < 1.0 {
            h = h.max(EXPANDED_COMPACT.1);
        }
        let (w, h) = (w * OVERSHOOT * scale, h * OVERSHOOT * scale);
        x0 = x0.min((max_w - w) / 2.0);
        x1 = x1.max(g.x0 + w);
        y1 = y1.max(g.y0 + h);
    }
    (x0 - margin, g.y0 - margin, x1 + margin, y1 + margin)
}

/// Como `bounds`, para la línea del borde superior y sus indicadores.
pub fn edge_strip_bounds(max_w: f32, scale: f32, _battery_low: bool) -> (f32, f32, f32, f32) {
    let w = edge_strip_width(scale);
    let x = (max_w - w) / 2.0;
    (x - 2.0 * scale, 0.0, x + w + 2.0 * scale, 5.0 * scale)
}

/// Borde inferior en píxeles físicos; debajo se coloca el proyector de la letra.
/// `shape` = (`lyrics_p`, `notice_p`), como en `geometry`.
pub fn bottom(max_w: f32, max_h: f32, scale: f32, p: f32, presence: f32, shape: (f32, f32)) -> f32 {
    let g = geometry(max_w, max_h, scale, p, presence, shape);
    g.y0 + g.h
}

/// Columna a la derecha de la carátula: (x, ancho). El texto nunca pasa del reproductor.
fn text_column(g: &Geometry, s: f32) -> (f32, f32) {
    let x = g.x0 + (COVER_PAD_X + COVER_SIZE + 14.0) * s;
    let w = (PLAYER_W - COVER_SIZE - 14.0) * s;
    (x, w.max(0.0))
}

/// Centros de los cuatro botones en la fila inferior del reproductor.
fn button_centers(
    g: &Geometry,
    s: f32,
    lyrics_available: bool,
) -> impl Iterator<Item = (Button, f32, f32)> {
    let px = g.x0 + COVER_PAD_X * s;
    let pw = PLAYER_W * s;
    let cy = g.y0 + CONTROLS_Y * s;
    let lyrics_cx = px + 14.0 * s;
    let inset = if lyrics_available { 44.0 * s } else { 0.0 };
    let media_cx = px + inset + (pw - inset) / 2.0;
    let gap = 40.0 * s;
    [
        (Button::Lyrics, lyrics_cx, cy),
        (Button::Prev, media_cx - gap, cy),
        (Button::PlayPause, media_cx, cy),
        (Button::Next, media_cx + gap, cy),
    ]
    .into_iter()
    .filter(move |(button, _, _)| *button != Button::Lyrics || lyrics_available)
}

/// Qué hay bajo el punto (x, y) de la ventana, solo si el contenido ya se ve por completo.
/// `seekable` = la barra de progreso está visible; `switchable` = hay otra app a la que cambiar;
/// `updatable` = se muestra el botón de actualización.
#[allow(clippy::too_many_arguments)]
pub fn hit_test(
    max_w: f32,
    max_h: f32,
    scale: f32,
    p: f32,
    presence: f32,
    shape: (f32, f32),
    seekable: bool,
    switchable: bool,
    updatable: bool,
    lyrics_available: bool,
    x: f32,
    y: f32,
) -> Option<Hit> {
    let g = geometry(max_w, max_h, scale, p, presence, shape);
    if g.alpha < 1.0 {
        return None;
    }
    if updatable {
        if let Some(hit) = indicator_hit_test(
            max_w,
            max_h,
            scale,
            p,
            presence,
            shape,
            Indicators {
                update: true,
                ..Indicators::default()
            },
            x,
            y,
        ) {
            return Some(hit);
        }
    }
    if switchable {
        let (cx, cy, size) = g.cover_rect();
        if x >= cx && x <= cx + size && y >= cy && y <= cy + size {
            return Some(Hit::Cover);
        }
    }
    if seekable {
        let (bx, by, bw) = g.progress_bar();
        let margin = 4.0 * scale;
        if (y - by).abs() <= PROGRESS_HIT * scale && x >= bx - margin && x <= bx + bw + margin {
            return Some(Hit::Seek(((x - bx) / bw.max(1.0)).clamp(0.0, 1.0)));
        }
    }
    button_centers(&g, scale, lyrics_available)
        .find(|&(button, bx, by)| {
            let r = if button == Button::PlayPause {
                19.0
            } else {
                BUTTON_RADIUS
            } * scale;
            (x - bx).powi(2) + (y - by).powi(2) <= r * r
        })
        .map(|(b, _, _)| Hit::Button(b))
}

/// Fracción de la canción bajo la `x` del mouse al arrastrar la barra de progreso, aunque el
/// mouse ya no esté encima de ella (se queda en los extremos).
pub fn seek_fraction(
    max_w: f32,
    max_h: f32,
    scale: f32,
    p: f32,
    presence: f32,
    shape: (f32, f32),
    x: f32,
) -> f32 {
    let g = geometry(max_w, max_h, scale, p, presence, shape);
    let (bx, _, bw) = g.progress_bar();
    ((x - bx) / bw.max(1.0)).clamp(0.0, 1.0)
}

/// Dibuja la isla con progreso `p` (0 = línea, 1 = expandida; puede pasarse un poco por el rebote).
/// `presence` = cuánto ha entrado en pantalla (ver `geometry`). `t` = segundos, para animar
/// las barras de audio. `shape` = (`lyrics_p`, `notice_p`), como en `geometry`.
/// Devuelve dónde dibujar el texto, o `None` si el contenido aún no se ve.
/// No limpia el `pixmap`: quien llama deja transparente al menos la zona de `bounds` (y la
/// del cuadro anterior), así no se recorre la ventana entera en cada cuadro.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    pixmap: &mut Pixmap,
    scale: f32,
    p: f32,
    presence: f32,
    shape: (f32, f32),
    controls: Option<Controls>,
    indicators: Indicators,
    notice: Option<(&Notice, f32)>,
    cover: Option<&Pixmap>,
    app_icon: Option<&Pixmap>,
    t: f32,
    time_info: Option<TimeInfo>,
    lyrics_info: Option<LyricsInfo>,
    translucent: bool,
    color: crate::settings::IslandColor,
    page: crate::workspace::Page,
) -> Option<LayoutAreas> {
    let g = geometry(
        pixmap.width() as f32,
        pixmap.height() as f32,
        scale,
        p,
        presence,
        shape,
    );
    // Mientras la isla pasa de la forma de aviso a la de la música, el reproductor espera a
    // que haya lugar para él (no se sale del cuadro negro).
    let g = match notice {
        Some(_) => g,
        None => Geometry {
            alpha: g.alpha * (1.0 - shape.1.clamp(0.0, 1.0)),
            ..g
        },
    };
    let lyrics_p = shape.0;
    let s = scale;

    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };

    let ([r, g_color, b], alpha) = if translucent {
        (color.blend(13, 18), 118)
    } else {
        (color.blend(8, 14), 255)
    };
    paint.set_color_rgba8(r, g_color, b, alpha);
    fill_rounded_rect(pixmap, &paint, g.x0, g.y0, g.w, g.h, g.r);

    let [r, g_color, b] = color.blend(255, 48);
    paint.set_color_rgba8(r, g_color, b, if translucent { 34 } else { 38 });
    if let Some(border) = rounded_rect_path(
        g.x0 + 0.5,
        g.y0 + 0.5,
        (g.w - 1.0).max(1.0),
        (g.h - 1.0).max(1.0),
        (g.r - 0.5).max(0.0),
    ) {
        stroke_path(pixmap, &paint, &border, 1.0);
    }

    if notice.is_none() {
        draw_indicators(pixmap, &g, p, indicators);
    }

    if g.alpha <= 0.0 {
        return None;
    }

    // Aviso del sistema
    if let Some(n) = notice {
        let text_area = draw_notice(pixmap, &g, n.0, n.1);
        return Some(LayoutAreas {
            labels: Vec::new(),
            text: text_area,
            time: None,
            lyrics: None,
            seek: None,
        });
    }

    if page == crate::workspace::Page::Share {
        return Some(LayoutAreas {
            labels: Vec::new(),
            text: TextArea {
                x: g.x0,
                w: 0.0,
                cy: g.cy(),
                alpha: 0.0,
            },
            time: None,
            lyrics: None,
            seek: None,
        });
    }
    // Panel izquierdo: carátula de la canción
    let (cover_x, cover_y, size) = g.cover_rect();
    draw_cover(
        pixmap,
        cover,
        cover_x,
        cover_y,
        size,
        COVER_RADIUS * s,
        g.alpha,
    );

    if let Some(c) = &controls {
        draw_bars(
            pixmap,
            g.x0 + (EXPANDED_COMPACT.0 - COVER_PAD_X - 20.0) * s,
            cover_y + size / 2.0,
            s,
            c,
            t,
            g.alpha,
        );
        let (index, count) = c.sessions;
        if c.cover_hovered && count > 1 {
            paint.set_color_rgba8(255, 255, 255, (g.alpha * 40.0) as u8);
            fill_rounded_rect(
                pixmap,
                &paint,
                cover_x,
                cover_y,
                size,
                size,
                COVER_RADIUS * s,
            );
        }
        if let Some(icon) = app_icon {
            draw_app_badge(pixmap, icon, cover_x + size, cover_y + size, s, g.alpha);
        }
        if count > 1 {
            draw_session_dots(pixmap, &g, index, count);
        }
    }

    // Botones de control
    let mut seek = None;
    if let Some(c) = &controls {
        for (button, cx, cy) in button_centers(&g, s, c.lyrics_available) {
            match c.pressed {
                Some((pressed, t)) if pressed == button => {
                    paint.set_color_rgba8(255, 255, 255, (g.alpha * 70.0 * (1.0 - t)) as u8);
                    fill_circle(pixmap, &paint, cx, cy, BUTTON_RADIUS * s * (0.9 + 0.6 * t));
                    let k = press_scale(t);
                    draw_button(pixmap, button, c, cx, cy, s * k, g.alpha, color);
                }
                _ => draw_button(pixmap, button, c, cx, cy, s, g.alpha, color),
            }
        }
        if let Some(progress) = c.progress {
            draw_progress(pixmap, &g, c.seek_hover.is_some(), progress, color);
            if let Some((fraction, label)) = &c.seek_hover {
                // Punto donde caería el salto; el tiempo lo escribe DirectWrite encima.
                let (bar_x, bar_cy, bar_w) = g.progress_bar();
                let x = bar_x + bar_w * fraction.clamp(0.0, 1.0);
                paint.set_color_rgba8(255, 255, 255, (g.alpha * 255.0) as u8);
                fill_circle(pixmap, &paint, x, bar_cy, 5.0 * s);
                seek = Some(SeekTip {
                    x,
                    bar_cy,
                    min_x: g.x0 + 8.0 * s,
                    max_x: g.x1() - 8.0 * s,
                    text: label.clone(),
                    alpha: g.alpha,
                });
            }
        }
    }

    // Áreas de texto: título/artista
    let (text_x, text_w) = text_column(&g, s);
    let title_area = TextArea {
        x: text_x,
        w: (text_w - if controls.is_some() { 26.0 * s } else { 0.0 }).max(0.0),
        cy: g.y0 + (COVER_PAD_TOP + HEADER_HEIGHT + COVER_SIZE / 2.0) * s,
        alpha: g.alpha,
    };

    // Áreas de texto: tiempos
    let time_area = time_info.map(|ti| {
        let px = g.x0 + COVER_PAD_X * s;
        let pw = PLAYER_W * s;
        TimeArea {
            cur_x: px,
            rem_x: px + pw - 38.0 * s,
            y: g.y0 + (PROGRESS_Y - 8.0) * s,
            current: ti.current,
            remaining: ti.remaining,
            alpha: g.alpha,
        }
    });

    // Áreas de texto: letras a la derecha cuando hay letra disponible
    let lyrics_area = if lyrics_p > 0.01 {
        lyrics_info.and_then(|li| {
            if li.lines.is_empty() && li.status.is_none() {
                None
            } else {
                let lyrics_x = g.x0 + (COVER_PAD_X + PLAYER_W + 18.0) * s;
                let lyrics_w = (g.x1() - lyrics_x - 18.0 * s).max(0.0);
                Some(LyricsArea {
                    x: lyrics_x,
                    w: lyrics_w,
                    cy: g.y0 + HEADER_HEIGHT * s + (g.h - HEADER_HEIGHT * s) / 2.0,
                    half_height: ((g.h - (HEADER_HEIGHT + 8.0) * s) / 2.0).clamp(0.0, 70.0 * s),
                    lines: li.lines,
                    shift: li.shift,
                    sung: li.sung,
                    status: li.status,
                    alpha: g.alpha * lyrics_p.clamp(0.0, 1.0),
                })
            }
        })
    } else {
        None
    };

    Some(LayoutAreas {
        labels: Vec::new(),
        text: title_area,
        time: time_area,
        lyrics: lyrics_area,
        seek,
    })
}

/// Aviso: ícono en un recuadro teñido donde va la carátula. Devuelve el espacio para los textos, que llega hasta el borde derecho (no hay botones).
/// `text_w`: ancho del texto más largo (píxeles físicos), para centrar ícono y texto juntos
/// en la isla; en vertical van al centro (los avisos no tienen barra de progreso debajo).
fn draw_notice(pixmap: &mut Pixmap, g: &Geometry, n: &Notice, text_w: f32) -> TextArea {
    draw_notice_content(pixmap, g, n, text_w, 1.0)
}

/// Tarjeta independiente con el mismo contenido del aviso, en una fila más baja.
pub fn draw_notice_card(
    pixmap: &mut Pixmap,
    scale: f32,
    notice: &Notice,
    text_w: f32,
) -> LayoutAreas {
    let g = Geometry {
        x0: 0.0,
        y0: 0.0,
        w: pixmap.width() as f32,
        h: pixmap.height() as f32,
        r: 16.0 * scale,
        alpha: 1.0,
        scale,
    };
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    paint.set_color_rgba8(20, 20, 24, 250);
    fill_rounded_rect(pixmap, &paint, g.x0, g.y0, g.w, g.h, g.r);
    if let Some(path) = rounded_rect_path(0.5, 0.5, g.w - 1.0, g.h - 1.0, g.r) {
        paint.set_color_rgba8(255, 255, 255, 36);
        stroke_path(pixmap, &paint, &path, 1.0);
    }
    LayoutAreas {
        labels: Vec::new(),
        text: draw_notice_content(pixmap, &g, notice, text_w, 0.8),
        time: None,
        lyrics: None,
        seek: None,
    }
}

fn draw_notice_content(
    pixmap: &mut Pixmap,
    g: &Geometry,
    n: &Notice,
    text_w: f32,
    icon_scale: f32,
) -> TextArea {
    let s = g.scale;
    let size = NOTICE_ICON.1 * s * icon_scale;
    let gap = 10.0 * s;
    let margin = NOTICE_ICON.0 * s;
    // Lo que cabe del texto entre los márgenes; si no cabe entero, se desplaza (marquee).
    let room = (g.w - 2.0 * margin - size - gap).max(0.0);
    let text_w = text_w.min(room);
    let x = g.x0 + (g.w - (size + gap + text_w)) / 2.0;
    let y = g.cy() - size / 2.0;
    let [r, gr, b] = n.color;
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    paint.set_color_rgba8(r, gr, b, (g.alpha * 46.0) as u8);
    fill_rounded_rect(
        pixmap,
        &paint,
        x,
        y,
        size,
        size,
        NOTICE_ICON.2 * s * icon_scale,
    );
    paint.set_color_rgba8(r, gr, b, (g.alpha * 255.0) as u8);
    draw_notice_icon(
        pixmap,
        &paint,
        n.icon,
        x + size / 2.0,
        y + size / 2.0,
        s * icon_scale,
    );
    TextArea {
        x: x + size + gap,
        // Un píxel de más: el ancho medido tiene decimales y el recorte no debe comerse la
        // última letra.
        w: text_w + 1.0,
        cy: g.cy(),
        alpha: g.alpha,
    }
}

/// Íconos de línea, de unos 20 px lógicos, centrados en (cx, cy).
pub(crate) fn draw_notice_icon(
    pixmap: &mut Pixmap,
    paint: &Paint,
    icon: NoticeIcon,
    cx: f32,
    cy: f32,
    s: f32,
) {
    let line = 1.8 * s;
    match icon {
        NoticeIcon::Music => {
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 3.0 * s, cy + 5.0 * s);
            pb.line_to(cx - 3.0 * s, cy - 6.0 * s);
            pb.line_to(cx + 7.0 * s, cy - 8.0 * s);
            pb.line_to(cx + 7.0 * s, cy + 3.0 * s);
            stroke(pixmap, paint, pb, line);
            fill_circle(pixmap, paint, cx - 5.5 * s, cy + 5.0 * s, 3.0 * s);
            fill_circle(pixmap, paint, cx + 4.5 * s, cy + 3.0 * s, 3.0 * s);
        }
        NoticeIcon::Bluetooth => {
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 5.0 * s, cy - 5.0 * s);
            pb.line_to(cx + 5.0 * s, cy + 5.0 * s);
            pb.line_to(cx, cy + 10.0 * s);
            pb.line_to(cx, cy - 10.0 * s);
            pb.line_to(cx + 5.0 * s, cy - 5.0 * s);
            pb.line_to(cx - 5.0 * s, cy + 5.0 * s);
            stroke(pixmap, paint, pb, line);
        }
        NoticeIcon::Share => {
            let nodes = [(-7.0, 0.0), (6.0, -7.0), (6.0, 7.0)];
            let mut path = PathBuilder::new();
            path.move_to(cx + 6.0 * s, cy - 7.0 * s);
            path.line_to(cx - 7.0 * s, cy);
            path.line_to(cx + 6.0 * s, cy + 7.0 * s);
            stroke(pixmap, paint, path, line);
            for (x, y) in nodes {
                fill_circle(pixmap, paint, cx + x * s, cy + y * s, 3.0 * s);
            }
        }
        NoticeIcon::Wireless => {
            let mut pb = PathBuilder::new();
            for (radius, y) in [(9.0, -3.5), (6.0, 0.0), (3.0, 3.5)] {
                pb.move_to(cx - radius * s, cy + y * s);
                pb.quad_to(cx, cy + (y - radius) * s, cx + radius * s, cy + y * s);
            }
            stroke(pixmap, paint, pb, line);
            fill_circle(pixmap, paint, cx, cy + 7.0 * s, 1.3 * s);
        }
        NoticeIcon::Usb => {
            let mut pb = PathBuilder::new();
            pb.move_to(cx, cy + 8.5 * s);
            pb.line_to(cx, cy - 8.0 * s);
            pb.move_to(cx - 2.8 * s, cy - 5.5 * s);
            pb.line_to(cx, cy - 8.5 * s);
            pb.line_to(cx + 2.8 * s, cy - 5.5 * s);
            pb.move_to(cx, cy + 4.5 * s);
            pb.line_to(cx - 5.0 * s, cy + s);
            pb.line_to(cx - 5.0 * s, cy - 2.0 * s);
            pb.move_to(cx, cy + s);
            pb.line_to(cx + 5.0 * s, cy - 2.5 * s);
            pb.line_to(cx + 5.0 * s, cy - 5.0 * s);
            stroke(pixmap, paint, pb, line);
            fill_circle(pixmap, paint, cx, cy + 8.0 * s, 1.8 * s);
        }
        NoticeIcon::Mouse => {
            if let Some(body) =
                rounded_rect_path(cx - 7.0 * s, cy - 10.0 * s, 14.0 * s, 21.0 * s, 7.0 * s)
            {
                stroke_path(pixmap, paint, &body, line);
            }
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 6.0 * s, cy - 3.0 * s);
            pb.line_to(cx + 6.0 * s, cy - 3.0 * s);
            pb.move_to(cx, cy - 7.0 * s);
            pb.line_to(cx, cy - 4.0 * s);
            stroke(pixmap, paint, pb, line);
        }
        NoticeIcon::Gamepad => {
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 5.0 * s, cy - 6.0 * s);
            pb.line_to(cx + 5.0 * s, cy - 6.0 * s);
            pb.cubic_to(
                cx + 10.0 * s,
                cy - 6.0 * s,
                cx + 10.0 * s,
                cy + 2.0 * s,
                cx + 11.0 * s,
                cy + 6.0 * s,
            );
            pb.cubic_to(
                cx + 11.0 * s,
                cy + 11.0 * s,
                cx + 5.0 * s,
                cy + 4.0 * s,
                cx + 3.0 * s,
                cy + 4.0 * s,
            );
            pb.line_to(cx - 3.0 * s, cy + 4.0 * s);
            pb.cubic_to(
                cx - 5.0 * s,
                cy + 4.0 * s,
                cx - 11.0 * s,
                cy + 11.0 * s,
                cx - 11.0 * s,
                cy + 6.0 * s,
            );
            pb.cubic_to(
                cx - 10.0 * s,
                cy + 2.0 * s,
                cx - 10.0 * s,
                cy - 6.0 * s,
                cx - 5.0 * s,
                cy - 6.0 * s,
            );
            pb.close();
            stroke(pixmap, paint, pb, line);
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 7.0 * s, cy - 1.0 * s);
            pb.line_to(cx - 2.0 * s, cy - 1.0 * s);
            pb.move_to(cx - 4.5 * s, cy - 3.5 * s);
            pb.line_to(cx - 4.5 * s, cy + 1.5 * s);
            stroke(pixmap, paint, pb, line);
            fill_circle(pixmap, paint, cx + 4.0 * s, cy + 0.5 * s, 1.2 * s);
            fill_circle(pixmap, paint, cx + 7.0 * s, cy - 2.5 * s, 1.2 * s);
        }
        NoticeIcon::Speaker => {
            if let Some(body) =
                rounded_rect_path(cx - 8.0 * s, cy - 10.0 * s, 16.0 * s, 20.0 * s, 2.5 * s)
            {
                stroke_path(pixmap, paint, &body, line);
            }
            if let Some(circle) = PathBuilder::from_circle(cx, cy + 3.0 * s, 4.0 * s) {
                stroke_path(pixmap, paint, &circle, line);
            }
            fill_circle(pixmap, paint, cx, cy - 5.0 * s, 1.6 * s);
        }
        NoticeIcon::Device => {
            if let Some(body) =
                rounded_rect_path(cx - 6.5 * s, cy - 10.0 * s, 13.0 * s, 20.0 * s, 2.5 * s)
            {
                stroke_path(pixmap, paint, &body, line);
            }
            fill_circle(pixmap, paint, cx, cy + 6.0 * s, 1.2 * s);
        }
        NoticeIcon::Battery => {
            if let Some(body) =
                rounded_rect_path(cx - 10.0 * s, cy - 6.0 * s, 18.0 * s, 12.0 * s, 2.0 * s)
            {
                stroke_path(pixmap, paint, &body, line);
            }
            fill_rounded_rect(
                pixmap,
                paint,
                cx + 9.0 * s,
                cy - 2.5 * s,
                2.0 * s,
                5.0 * s,
                s,
            );
            fill_rounded_rect(
                pixmap,
                paint,
                cx - 7.0 * s,
                cy - 3.0 * s,
                3.0 * s,
                6.0 * s,
                s,
            );
        }
        NoticeIcon::Audio => {
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 8.0 * s, cy + 3.0 * s);
            pb.line_to(cx - 8.0 * s, cy - 1.0 * s);
            pb.cubic_to(
                cx - 8.0 * s,
                cy - 11.0 * s,
                cx + 8.0 * s,
                cy - 11.0 * s,
                cx + 8.0 * s,
                cy - 1.0 * s,
            );
            pb.line_to(cx + 8.0 * s, cy + 3.0 * s);
            stroke(pixmap, paint, pb, line);
            fill_rounded_rect(pixmap, paint, cx - 10.0 * s, cy, 5.0 * s, 9.0 * s, 2.0 * s);
            fill_rounded_rect(pixmap, paint, cx + 5.0 * s, cy, 5.0 * s, 9.0 * s, 2.0 * s);
        }
        NoticeIcon::Timer => {
            let cy = cy + 1.0 * s;
            if let Some(circle) = PathBuilder::from_circle(cx, cy, 8.0 * s) {
                stroke_path(pixmap, paint, &circle, line);
            }
            let mut pb = PathBuilder::new();
            pb.move_to(cx, cy);
            pb.line_to(cx, cy - 4.5 * s);
            pb.move_to(cx - 2.5 * s, cy - 11.0 * s);
            pb.line_to(cx + 2.5 * s, cy - 11.0 * s);
            stroke(pixmap, paint, pb, line);
        }
        NoticeIcon::Keyboard => {
            if let Some(body) =
                rounded_rect_path(cx - 10.0 * s, cy - 7.0 * s, 20.0 * s, 14.0 * s, 3.0 * s)
            {
                stroke_path(pixmap, paint, &body, 1.6 * s);
            }
            for row in [-3.0, 0.5] {
                for col in [-6.0, -2.0, 2.0, 6.0] {
                    fill_circle(pixmap, paint, cx + col * s, cy + row * s, 1.1 * s);
                }
            }
            fill_rounded_rect(
                pixmap,
                paint,
                cx - 5.0 * s,
                cy + 3.3 * s,
                10.0 * s,
                1.6 * s,
                0.8 * s,
            );
        }
        NoticeIcon::Installing => {
            let mut pb = PathBuilder::new();
            pb.move_to(cx, cy - 8.0 * s);
            pb.line_to(cx, cy + 3.0 * s);
            pb.move_to(cx - 4.5 * s, cy - 1.5 * s);
            pb.line_to(cx, cy + 3.0 * s);
            pb.line_to(cx + 4.5 * s, cy - 1.5 * s);
            pb.move_to(cx - 8.0 * s, cy + 8.0 * s);
            pb.line_to(cx + 8.0 * s, cy + 8.0 * s);
            stroke(pixmap, paint, pb, line);
        }
        NoticeIcon::Done => {
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 7.0 * s, cy);
            pb.line_to(cx - 2.0 * s, cy + 5.0 * s);
            pb.line_to(cx + 8.0 * s, cy - 6.0 * s);
            stroke(pixmap, paint, pb, 2.2 * s);
        }
        NoticeIcon::Failed => {
            let mut pb = PathBuilder::new();
            pb.move_to(cx, cy - 8.0 * s);
            pb.line_to(cx, cy + 2.5 * s);
            stroke(pixmap, paint, pb, 2.2 * s);
            fill_circle(pixmap, paint, cx, cy + 7.0 * s, 1.5 * s);
        }
    }
}

/// Traza las líneas de `pb` con puntas redondeadas.
pub(crate) fn stroke(pixmap: &mut Pixmap, paint: &Paint, pb: PathBuilder, width: f32) {
    if let Some(path) = pb.finish() {
        stroke_path(pixmap, paint, &path, width);
    }
}

/// Dibuja un trazado ya construido con el mismo estilo de puntas redondeadas.
pub(crate) fn stroke_path(pixmap: &mut Pixmap, paint: &Paint, path: &Path, width: f32) {
    let stroke = tiny_skia::Stroke {
        width,
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        ..Default::default()
    };
    pixmap.stroke_path(path, paint, &stroke, Transform::identity(), None);
}

/// Insignia con el ícono de la app, montada sobre la esquina inferior derecha de la carátula
/// (`(x, y)` = esa esquina). Un marco negro la separa de la imagen.
fn draw_app_badge(pixmap: &mut Pixmap, icon: &Pixmap, x: f32, y: f32, s: f32, alpha: f32) {
    let (frame, inner) = (18.0 * s, 14.0 * s);
    let (fx, fy) = (x - frame + 4.0 * s, y - frame + 4.0 * s);
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    paint.set_color_rgba8(0, 0, 0, (alpha * 255.0) as u8);
    fill_rounded_rect(pixmap, &paint, fx, fy, frame, frame, 5.0 * s);
    let pad = (frame - inner) / 2.0;
    draw_cover(
        pixmap,
        Some(icon),
        fx + pad,
        fy + pad,
        inner,
        3.5 * s,
        alpha,
    );
}

/// Puntos bajo la carátula: uno por app con música; el de la mostrada, blanco.
fn draw_session_dots(pixmap: &mut Pixmap, g: &Geometry, index: usize, count: usize) {
    let s = g.scale;
    let (cover_x, cover_y, size) = g.cover_rect();
    let (r, gap) = (1.5 * s, 6.0 * s);
    let cy = cover_y + size + 5.0 * s;
    let first = cover_x + size / 2.0 - gap * (count - 1) as f32 / 2.0;
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    for i in 0..count {
        let a = if i == index { 255.0 } else { 90.0 };
        paint.set_color_rgba8(255, 255, 255, (g.alpha * a) as u8);
        fill_circle(pixmap, &paint, first + gap * i as f32, cy, r);
    }
}

/// Barra inferior: pista gris y nivel `fraction` en blanco.
/// `thick` la engrosa cuando el mouse está encima.
fn draw_progress(
    pixmap: &mut Pixmap,
    g: &Geometry,
    thick: bool,
    fraction: f32,
    color: crate::settings::IslandColor,
) {
    let (bar_x, bar_cy, bar_w) = g.progress_bar();
    let h = if thick {
        PROGRESS_THICK.1
    } else {
        PROGRESS_THICK.0
    } * g.scale;
    let y = bar_cy - h / 2.0;
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    paint.set_color_rgba8(255, 255, 255, (g.alpha * 48.0) as u8);
    fill_rounded_rect(pixmap, &paint, bar_x, y, bar_w, h, h / 2.0);

    let [r, g_color, b] = color.blend(255, 42);
    paint.set_color_rgba8(r, g_color, b, (g.alpha * 255.0) as u8);
    let filled = (bar_w * fraction.clamp(0.0, 1.0)).max(h);
    fill_rounded_rect(pixmap, &paint, bar_x, y, filled, h, h / 2.0);
}

/// Carátula con esquinas redondeadas; sin imagen, un marcador gris.
fn draw_cover(
    pixmap: &mut Pixmap,
    cover: Option<&Pixmap>,
    x: f32,
    y: f32,
    size: f32,
    radius: f32,
    alpha: f32,
) {
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    match cover {
        Some(img) => {
            let k = size / img.width() as f32;
            paint.shader = Pattern::new(
                img.as_ref(),
                SpreadMode::Pad,
                FilterQuality::Bicubic,
                alpha,
                Transform::from_scale(k, k).post_translate(x, y),
            );
        }
        None => paint.set_color_rgba8(48, 48, 52, (alpha * 255.0) as u8),
    }
    fill_rounded_rect(pixmap, &paint, x, y, size, size, radius);
}

/// Velocidades (rad/s) de las dos ondas que mueven cada barra; distintas para que no vayan al compás.
const BAR_SPEEDS: [(f32, f32); 4] = [(7.1, 3.3), (9.3, 4.7), (6.2, 5.9), (8.4, 2.8)];

/// Barras de audio junto al título en el reproductor. En pausa quedan quietas y bajas.
fn draw_bars(pixmap: &mut Pixmap, left: f32, cy: f32, s: f32, c: &Controls, t: f32, alpha: f32) {
    let (bar_w, gap) = (3.0 * s, 2.5 * s);
    let (min_h, max_h) = (3.0 * s, 22.0 * s);

    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    let [r, gr, b] = c.accent;
    paint.set_color_rgba8(r, gr, b, (alpha * 255.0) as u8);

    for (i, &(w1, w2)) in BAR_SPEEDS.iter().enumerate() {
        let level = if let (true, Some(levels)) = (c.playing, c.levels) {
            levels[i].clamp(0.0, 1.0)
        } else if c.playing {
            let k = i as f32;
            (0.5 + 0.3 * (t * w1 + k).sin() + 0.2 * (t * w2 + 2.0 * k).sin()).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let h = min_h + (max_h - min_h) * level;
        let x = left + i as f32 * (bar_w + gap);
        fill_rounded_rect(pixmap, &paint, x, cy - h / 2.0, bar_w, h, bar_w / 2.0);
    }
}

/// Íconos blancos del reproductor; el botón de letra indica si está activa.
/// El círculo tenue de fondo resalta el botón bajo el mouse.
#[allow(clippy::too_many_arguments)]
fn draw_button(
    pixmap: &mut Pixmap,
    button: Button,
    c: &Controls,
    cx: f32,
    cy: f32,
    s: f32,
    alpha: f32,
    color: crate::settings::IslandColor,
) {
    let a = (alpha * 255.0) as u8;
    let hovered = c.hovered == Some(button);
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };

    if button == Button::PlayPause {
        let [r, g, b] = color.blend(35, 55);
        paint.set_color_rgba8(r, g, b, (alpha * if hovered { 125.0 } else { 78.0 }) as u8);
        fill_circle(pixmap, &paint, cx, cy, 19.0 * s);
    } else if hovered {
        paint.set_color_rgba8(255, 255, 255, (alpha * 35.0) as u8);
        fill_circle(pixmap, &paint, cx, cy, BUTTON_RADIUS * s);
    }

    let [r, g, b] = color.blend(255, 42);
    match button {
        Button::Lyrics => {
            let bubble_a = if c.lyrics_active {
                a
            } else {
                (alpha * 125.0) as u8
            };
            paint.set_color_rgba8(r, g, b, bubble_a);

            let bw = 16.0 * s;
            let bh = 11.5 * s;
            let bx = cx - bw / 2.0;
            let by = cy - bh / 2.0 - 1.0 * s;

            if c.lyrics_active {
                fill_rounded_rect(pixmap, &paint, bx, by, bw, bh, 3.5 * s);
                let mut pb = PathBuilder::new();
                pb.move_to(bx + 2.5 * s, by + bh - 1.0 * s);
                pb.line_to(bx - 1.0 * s, by + bh + 3.2 * s);
                pb.line_to(bx + 6.0 * s, by + bh - 1.0 * s);
                pb.close();
                if let Some(path) = pb.finish() {
                    pixmap.fill_path(
                        &path,
                        &paint,
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
                paint.set_color_rgba8(0, 0, 0, a);
                fill_rounded_rect(
                    pixmap,
                    &paint,
                    cx - 3.8 * s,
                    cy - 3.2 * s,
                    2.0 * s,
                    3.2 * s,
                    0.8 * s,
                );
                fill_rounded_rect(
                    pixmap,
                    &paint,
                    cx + 1.6 * s,
                    cy - 3.2 * s,
                    2.0 * s,
                    3.2 * s,
                    0.8 * s,
                );
            } else {
                if let Some(body) = rounded_rect_path(bx, by, bw, bh, 3.5 * s) {
                    stroke_path(pixmap, &paint, &body, 1.4 * s);
                }
                let mut pb = PathBuilder::new();
                pb.move_to(bx + 2.5 * s, by + bh - 0.5 * s);
                pb.line_to(bx - 1.0 * s, by + bh + 3.2 * s);
                pb.line_to(bx + 6.0 * s, by + bh - 0.5 * s);
                stroke(pixmap, &paint, pb, 1.4 * s);
                fill_rounded_rect(
                    pixmap,
                    &paint,
                    cx - 3.8 * s,
                    cy - 3.2 * s,
                    1.8 * s,
                    2.8 * s,
                    0.6 * s,
                );
                fill_rounded_rect(
                    pixmap,
                    &paint,
                    cx + 1.6 * s,
                    cy - 3.2 * s,
                    1.8 * s,
                    2.8 * s,
                    0.6 * s,
                );
            }
        }
        Button::PlayPause => {
            paint.set_color_rgba8(r, g, b, a);
            if c.playing {
                let bar_w = 3.6 * s;
                let bar_h = 13.0 * s;
                let r = 1.2 * s;
                fill_rounded_rect(
                    pixmap,
                    &paint,
                    cx - 4.8 * s,
                    cy - bar_h / 2.0,
                    bar_w,
                    bar_h,
                    r,
                );
                fill_rounded_rect(
                    pixmap,
                    &paint,
                    cx + 1.2 * s,
                    cy - bar_h / 2.0,
                    bar_w,
                    bar_h,
                    r,
                );
            } else {
                fill_triangle(pixmap, &paint, cx - 4.5 * s, cy, 11.5 * s, 13.0 * s);
            }
        }
        Button::Prev => {
            paint.set_color_rgba8(r, g, b, a);
            let tri_w = 6.0 * s;
            let tri_h = 10.0 * s;
            fill_triangle(pixmap, &paint, cx + 1.0 * s, cy, -tri_w, tri_h);
            fill_triangle(pixmap, &paint, cx + 7.5 * s, cy, -tri_w, tri_h);
        }
        Button::Next => {
            paint.set_color_rgba8(r, g, b, a);
            let tri_w = 6.0 * s;
            let tri_h = 10.0 * s;
            fill_triangle(pixmap, &paint, cx - 7.5 * s, cy, tri_w, tri_h);
            fill_triangle(pixmap, &paint, cx - 1.0 * s, cy, tri_w, tri_h);
        }
    }
}

fn fill_circle(pixmap: &mut Pixmap, paint: &Paint, cx: f32, cy: f32, r: f32) {
    if let Some(circle) = PathBuilder::from_circle(cx, cy, r) {
        pixmap.fill_path(
            &circle,
            paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// Triángulo con la base vertical en `x` (alto `h`, centrado en `cy`) y la punta en `x + w`.
/// Con `w` negativo apunta a la izquierda.
fn fill_triangle(pixmap: &mut Pixmap, paint: &Paint, x: f32, cy: f32, w: f32, h: f32) {
    let mut pb = PathBuilder::new();
    pb.move_to(x, cy - h / 2.0);
    pb.line_to(x + w, cy);
    pb.line_to(x, cy + h / 2.0);
    pb.close();
    if let Some(path) = pb.finish() {
        pixmap.fill_path(&path, paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// Constante para aproximar un cuarto de círculo con una curva cúbica.
const KAPPA: f32 = 0.552_284_8;

/// Rellena un rectángulo con las cuatro esquinas redondeadas.
pub(crate) fn fill_rounded_rect(
    pixmap: &mut Pixmap,
    paint: &Paint,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
) {
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        pixmap.fill_path(&path, paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// Ícono de la bandeja: una mini isla negra con borde gris, visible en barras claras y oscuras.
pub fn tray_icon(size: u32) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(size, size)?;
    let s = size as f32;
    let line = (s / 16.0).max(1.0);
    let (h, y) = (s * 0.44, s * 0.28);
    let path = rounded_rect_path(line / 2.0, y, s - line, h, h / 2.0)?;
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    paint.set_color_rgba8(0, 0, 0, 255);
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    paint.set_color_rgba8(170, 170, 176, 255);
    let stroke = tiny_skia::Stroke {
        width: line,
        ..Default::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    Some(pixmap)
}

/// Construye el contorno redondeado para reutilizarlo como relleno, borde o recorte.
pub(crate) fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w / 2.0).min(h / 2.0);
    let k = r * KAPPA;
    let (x1, y1) = (x + w, y + h);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x1 - r, y);
    pb.cubic_to(x1 - r + k, y, x1, y + r - k, x1, y + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x + r, y1);
    pb.cubic_to(x + r - k, y1, x, y1 - r + k, x, y1 - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iconos_de_dispositivo_son_distintos_y_caben_a_distintas_escalas() {
        use crate::battery::DeviceKind;
        let kinds = [
            DeviceKind::Mouse,
            DeviceKind::Keyboard,
            DeviceKind::Headphones,
            DeviceKind::Speaker,
            DeviceKind::Gamepad,
            DeviceKind::Unknown,
        ];
        for scale in [1.0, 1.25, 2.0] {
            let size = (32.0_f32 * scale).ceil() as u32;
            let mut drawings = std::collections::HashSet::new();
            let mut paint = Paint::default();
            paint.set_color_rgba8(255, 255, 255, 255);
            for kind in kinds {
                let mut image = Pixmap::new(size, size).unwrap();
                draw_notice_icon(
                    &mut image,
                    &paint,
                    device_icon(kind),
                    size as f32 / 2.0,
                    size as f32 / 2.0,
                    scale,
                );
                assert!(image.pixels().iter().any(|p| p.alpha() != 0));
                for x in 0..size {
                    assert_eq!(image.pixel(x, 0).unwrap().alpha(), 0);
                    assert_eq!(image.pixel(x, size - 1).unwrap().alpha(), 0);
                }
                for y in 0..size {
                    assert_eq!(image.pixel(0, y).unwrap().alpha(), 0);
                    assert_eq!(image.pixel(size - 1, y).unwrap().alpha(), 0);
                }
                assert!(
                    drawings.insert(image.data().to_vec()),
                    "dos tipos comparten el mismo dibujo"
                );
            }
        }
    }
    use std::time::Instant;

    fn controls() -> Controls {
        Controls {
            playing: true,
            hovered: None,
            seek_hover: None,
            accent: [200, 120, 60],
            progress: Some(0.4),
            sessions: (0, 1),
            cover_hovered: false,
            pressed: None,
            levels: Some([0.6, 0.4, 0.3, 0.2]),
            lyrics_active: false,
            lyrics_available: false,
        }
    }

    #[test]
    fn barras_reales_ocupan_su_espacio_en_el_reproductor() {
        for scale in [1.0, 1.25, 2.0] {
            let (w, h) = window_size();
            let (w, h) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
            for lyrics in [0.0, 1.0] {
                for (page, p, playing, changes) in [
                    (crate::workspace::Page::Music, 1.0, true, true),
                    (crate::workspace::Page::Music, 1.0, false, false),
                    (crate::workspace::Page::Music, 0.0, true, false),
                    (crate::workspace::Page::Share, 1.0, true, false),
                ] {
                    let render = |level| {
                        let mut image = Pixmap::new(w, h).unwrap();
                        let mut c = controls();
                        c.playing = playing;
                        c.levels = Some([level; 4]);
                        let areas = draw(
                            &mut image,
                            scale,
                            p,
                            1.0,
                            (lyrics, 0.0),
                            Some(c),
                            Indicators::default(),
                            None,
                            None,
                            None,
                            0.0,
                            None,
                            None,
                            false,
                            crate::settings::IslandColor::default(),
                            page,
                        );
                        (image, areas)
                    };
                    let (quiet, _) = render(0.0);
                    let (loud, areas) = render(1.0);
                    let mut changed = false;
                    for (i, (a, b)) in quiet.pixels().iter().zip(loud.pixels()).enumerate() {
                        if a == b {
                            continue;
                        }
                        changed = true;
                        let x = (i as u32 % w) as f32;
                        let y = (i as u32 / w) as f32;
                        let text = &areas.as_ref().unwrap().text;
                        let g = geometry(w as f32, h as f32, scale, p, 1.0, (lyrics, 0.0));
                        let (_, top, size) = g.cover_rect();
                        assert!(x > text.x + text.w, "las barras invaden el título");
                        assert!(x < g.x0 + (EXPANDED_COMPACT.0 - COVER_PAD_X) * scale);
                        assert!(
                            y >= top && y <= top + size,
                            "las barras invaden los controles"
                        );
                    }
                    assert_eq!(changed, changes);
                }
            }
        }
    }

    #[test]
    fn letra_solo_se_dibuja_y_recibe_clics_cuando_esta_disponible() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let (w, h) = window_size();
            let (w, h) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
            let g = geometry(w as f32, h as f32, scale, 1.0, 1.0, (0.0, 0.0));
            let (_, lx, ly) = button_centers(&g, scale, true).next().unwrap();
            for available in [false, true] {
                let buttons: Vec<_> = button_centers(&g, scale, available).collect();
                assert_eq!(buttons.len(), if available { 4 } else { 3 });
                let mut image = Pixmap::new(w, h).unwrap();
                let mut c = controls();
                c.lyrics_available = available;
                for (button, x, y) in buttons {
                    draw_button(
                        &mut image,
                        button,
                        &c,
                        x,
                        y,
                        scale,
                        1.0,
                        crate::settings::IslandColor::default(),
                    );
                    assert!(
                        hit_test(
                            w as f32,
                            h as f32,
                            scale,
                            1.0,
                            1.0,
                            (0.0, 0.0),
                            false,
                            false,
                            false,
                            available,
                            x,
                            y
                        ) == Some(Hit::Button(button))
                    );
                    if button == Button::PlayPause && !available {
                        assert!((x - (g.x0 + g.w / 2.0)).abs() < 0.01);
                    }
                }
                assert_eq!(
                    image
                        .pixel((lx - 3.0 * scale) as u32, (ly - 2.0 * scale) as u32)
                        .unwrap()
                        .alpha()
                        > 0,
                    available
                );
                let hit = hit_test(
                    w as f32,
                    h as f32,
                    scale,
                    1.0,
                    1.0,
                    (0.0, 0.0),
                    false,
                    false,
                    false,
                    available,
                    lx,
                    ly,
                );
                assert_eq!(hit == Some(Hit::Button(Button::Lyrics)), available);
            }
        }
    }

    /// Milisegundos promedio de un cuadro de `draw` en una ventana a escala 1,25.
    fn frame_ms(p: f32) -> f64 {
        let (w, h) = window_size();
        let mut pixmap = Pixmap::new((w * 1.25).ceil() as u32, (h * 1.25).ceil() as u32).unwrap();
        let runs = 200;
        let start = Instant::now();
        for i in 0..runs {
            draw(
                &mut pixmap,
                1.25,
                p,
                1.0,
                (0.0, 0.0),
                Some(controls()),
                Indicators::default(),
                None,
                None,
                None,
                i as f32 / 30.0,
                None,
                None,
                false,
                crate::settings::IslandColor::default(),
                crate::workspace::Page::Music,
            );
        }
        start.elapsed().as_secs_f64() * 1000.0 / runs as f64
    }

    /// Cuánto cuesta dibujar la línea y el reproductor extendido con barras.
    /// Con margen de sobra para equipos lentos; sin optimizar (tests en debug), 20 veces más.
    #[test]
    fn dibujar_un_cuadro_es_barato() {
        let slack = if cfg!(debug_assertions) { 20.0 } else { 1.0 };
        let (collapsed, expanded) = (frame_ms(0.0), frame_ms(1.0));
        println!("contraida: {collapsed:.3} ms, expandida: {expanded:.3} ms");
        assert!(collapsed < 0.5 * slack, "contraida: {collapsed:.3} ms");
        assert!(expanded < 2.0 * slack, "expandida: {expanded:.3} ms");
    }

    #[test]
    fn indicadores_quedan_dentro_del_contorno_y_responden_sin_musica() {
        let indicators = Indicators {
            battery_low: true,
            update: true,
            ..Indicators::default()
        };
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let (w, h) = window_size();
            let (w, h) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
            for step in 0..=22 {
                let p = step as f32 * 0.05;
                let g = geometry(w as f32, h as f32, scale, p, 1.0, (0.0, 0.0));
                let mut image = Pixmap::new(w, h).unwrap();
                draw_indicators(&mut image, &g, p, indicators);
                for (index, pixel) in image.pixels().iter().enumerate() {
                    if pixel.alpha() == 0 {
                        continue;
                    }
                    let x = (index as u32 % w) as f32 + 0.5;
                    let y = (index as u32 / w) as f32 + 0.5;
                    let cx = x.clamp(g.x0 + g.r, g.x1() - g.r);
                    let cy = y.clamp(g.y0 + g.r, g.y0 + g.h - g.r);
                    assert!(
                        (x - cx).hypot(y - cy) <= g.r + 0.75,
                        "indicador fuera del contorno: escala={scale}, progreso={p}, ({x},{y})"
                    );
                }
                for (battery, hit) in [(true, Hit::Battery), (false, Hit::Update)] {
                    let (x, y) = indicator_center(&g, p, battery);
                    assert!(
                        indicator_hit_test(
                            w as f32,
                            h as f32,
                            scale,
                            p,
                            1.0,
                            (0.0, 0.0),
                            indicators,
                            x,
                            y
                        ) == if g.alpha >= 1.0 { Some(hit) } else { None }
                    );
                }
            }
        }
    }

    #[test]
    fn bateria_se_integra_dentro_del_extremo_de_la_franja() {
        let scale = 1.25;
        let (w, h) = window_size();
        let mut pixmap = Pixmap::new((w * scale).ceil() as u32, (h * scale).ceil() as u32).unwrap();
        let width = pixmap.width() as f32;
        let (left, top, right, bottom) = edge_strip_bounds(width, scale, true);
        draw_edge_battery_marker(&mut pixmap, scale);
        let mut visible = false;
        for (index, px) in pixmap.pixels().iter().enumerate() {
            if px.alpha() == 0 {
                continue;
            }
            visible = true;
            let x = (index as u32 % pixmap.width()) as f32;
            let y = (index as u32 / pixmap.width()) as f32;
            assert!(x >= left.floor() && x < right.ceil() && y >= top && y < bottom.ceil());
            assert!(x >= (width - edge_strip_width(scale)) / 2.0);
            assert!(px.red() > px.green() && px.green() > px.blue());
        }
        assert!(visible);
    }

    /// En cada paso de la animación, todo lo que se pinta cae dentro de `bounds`: si no, esa
    /// zona no se limpia en el cuadro siguiente y quedan restos de la isla expandida.
    #[test]
    fn lo_pintado_cabe_en_la_zona() {
        let scale = 1.25;
        let (w, h) = window_size();
        let (pw, ph) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
        let notice = Notice {
            icon: NoticeIcon::Audio,
            color: [255, 255, 255],
        };
        for shape in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)] {
            for step in 0..=22 {
                let p = step as f32 * 0.05;
                for presence in [0.4, 1.0] {
                    let mut pixmap = Pixmap::new(pw, ph).unwrap();
                    let c = controls();
                    let notice = (shape.1 > 0.0).then_some((&notice, 1.0));
                    draw(
                        &mut pixmap,
                        scale,
                        p,
                        presence,
                        shape,
                        Some(c),
                        Indicators {
                            battery_low: true,
                            update: true,
                            ..Indicators::default()
                        },
                        notice,
                        None,
                        None,
                        0.0,
                        None,
                        None,
                        false,
                        crate::settings::IslandColor::default(),
                        crate::workspace::Page::Music,
                    );
                    let (x0, y0, x1, y1) = bounds(pw as f32, ph as f32, scale, p, presence, shape);
                    for (i, px) in pixmap.pixels().iter().enumerate() {
                        let (x, y) = ((i as u32 % pw) as f32, (i as u32 / pw) as f32);
                        assert!(
                            px.alpha() == 0 || (x >= x0.floor() && x < x1.ceil() && y >= y0.floor() && y < y1.ceil()),
                            "p = {p}, presence = {presence}, shape = {shape:?}: píxel ({x}, {y}) fuera de {:?}",
                            (x0, y0, x1, y1)
                        );
                    }
                }
            }
        }
    }
}
