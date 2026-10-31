//! Texto con DirectWrite, dibujado por Direct2D directamente sobre el DIB de la ventana.
//!
//! Se usa el modo software de Direct2D: no carga drivers de GPU (menos RAM) y para unas
//! pocas líneas de texto es igual de rápido.
//!
//! Si una línea no cabe en su columna se desplaza de un lado al otro (marquee): pausa al
//! inicio, avanza hasta mostrar el final, pausa, y vuelve.
//! Además maquetamos y dibujamos las etiquetas de tiempo y la letra sincronizada a la derecha.

use windows::core::{w, Result};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1RenderTarget, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_REGULAR,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_HIT_TEST_METRICS, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP,
    DWRITE_WORD_WRAPPING_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmap, IWICImagingFactory,
    WICBitmapCacheOnDemand, WICBitmapLockRead, WICRect,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows_numerics::{Matrix3x2, Vector2};

use crate::island::{LayoutAreas, TextArea};

/// Velocidad lógica del desplazamiento, en píxeles por segundo.
const SCROLL_SPEED: f32 = 30.0;
/// Pausa en cada extremo, en segundos.
const SCROLL_PAUSE: f32 = 1.5;

/// Tamaño lógico de letra del verso que se canta (negrita) y de los demás.
const LYRIC_BOLD_SIZE: f32 = 15.5;
const LYRIC_SIZE: f32 = 12.5;
/// Lugar de cada verso del carrusel según su distancia al que se canta (de -2 a 3): borde
/// superior respecto del centro de la columna, opacidad y tamaño de letra, en píxeles lógicos.
/// Entre dos lugares se interpola: así los versos suben, crecen y se aclaran de forma continua.
const LYRIC_SLOTS: [(f32, f32, f32); 6] = [
    // Sale por arriba.
    (-64.0, 0.0, 11.0),
    (-44.0, 0.40, LYRIC_SIZE),
    // El que se canta: puede ocupar dos renglones.
    (-20.0, 1.0, LYRIC_BOLD_SIZE),
    (24.0, 0.35, LYRIC_SIZE),
    (45.0, 0.20, 11.0),
    // Entra por abajo.
    (62.0, 0.0, 10.0),
];
/// Versos maquetados que se guardan: los seis del carrusel en sus dos formatos, más el aviso.
const LYRIC_CACHE: usize = 14;
/// Opacidad de la parte del verso actual que aún no se canta (la cantada va en blanco pleno).
pub(crate) const UNSUNG_ALPHA: f32 = 0.35;
/// Límites del intervalo entre cuadros mientras el texto avanza (~60 y ~30 fps).
const SCROLL_FRAME_MS: (u32, u32) = (15, 33);

/// Una línea ya maquetada, guardada mientras el texto no cambie.
struct Line {
    text: String,
    layout: IDWriteTextLayout,
    width: f32,
}

/// Formatos, layouts y pincel reutilizables; se invalidan los textos al cambiar el DPI.
///
/// El texto se dibuja en una imagen propia (WIC, en memoria) y después se mezcla sobre el
/// DIB de la ventana, solo en la zona que cambió. Dibujar directo sobre el DIB (un "DC render
/// target") copia la ventana entera de ida y vuelta en cada cuadro: costaba casi 1 ms aunque
/// no hubiera texto.
pub struct TextRenderer {
    dwrite: IDWriteFactory,
    d2d: ID2D1Factory,
    wic: IWICImagingFactory,
    /// Imagen donde se dibuja el texto, del tamaño de la ventana (se crea al primer cuadro).
    canvas: Option<IWICBitmap>,
    canvas_size: (u32, u32),
    /// Qué se dibujó en la imagen en el último cuadro (firma de textos, posiciones y zona):
    /// si el cuadro nuevo es igual, se reutiliza sin volver a dibujar las letras.
    drawn: Option<(u64, (u32, u32, u32, u32))>,
    target: ID2D1RenderTarget,
    title: IDWriteTextFormat,
    subtitle: IDWriteTextFormat,
    time: IDWriteTextFormat,
    lyric_current: IDWriteTextFormat,
    lyric_secondary: IDWriteTextFormat,
    title_line: Option<Line>,
    subtitle_line: Option<Line>,
    // Una entrada por etiqueta: memoria acotada aunque cambien canción y tiempos.
    time_lines: [Option<Line>; 2],
    /// Tiempo del globo sobre la barra de progreso.
    seek_line: Option<Line>,
    /// Versos del carrusel ya maquetados, en negrita (`true`) o normal; los más viejos primero.
    lyric_cache: Vec<(bool, Line)>,
    brush: ID2D1SolidColorBrush,
    scale: f32,
}

impl TextRenderer {
    /// Crea los formatos y un pincel compartido para todos los textos de la isla.
    pub fn new(scale: f32) -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            // Imagen mínima hasta el primer cuadro, que la crea del tamaño de la ventana.
            let canvas =
                wic.CreateBitmap(1, 1, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand)?;
            let (target, brush) = make_target(&d2d, &canvas)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let title = make_format(&dwrite, DWRITE_FONT_WEIGHT_SEMI_BOLD, 14.5 * scale)?;
            let subtitle = make_format(&dwrite, DWRITE_FONT_WEIGHT_REGULAR, 12.0 * scale)?;
            let time = make_format(&dwrite, DWRITE_FONT_WEIGHT_REGULAR, 11.5 * scale)?;
            let lyric_current = make_format_wrapped(
                &dwrite,
                DWRITE_FONT_WEIGHT_BOLD,
                LYRIC_BOLD_SIZE * scale,
                true,
            )?;
            let lyric_secondary =
                make_format(&dwrite, DWRITE_FONT_WEIGHT_REGULAR, LYRIC_SIZE * scale)?;

            Ok(Self {
                dwrite,
                d2d,
                wic,
                canvas: Some(canvas),
                canvas_size: (1, 1),
                drawn: None,
                target,
                title,
                subtitle,
                time,
                lyric_current,
                lyric_secondary,
                brush,
                time_lines: [None, None],
                seek_line: None,
                lyric_cache: Vec::with_capacity(LYRIC_CACHE),
                title_line: None,
                subtitle_line: None,
                scale,
            })
        }
    }

    /// Cambió el DPI: los tamaños de letra se recalculan en píxeles físicos.
    pub fn set_scale(&mut self, scale: f32) {
        if scale == self.scale {
            return;
        }
        if let (Ok(title), Ok(subtitle), Ok(time), Ok(lyric_current), Ok(lyric_secondary)) = (
            make_format(&self.dwrite, DWRITE_FONT_WEIGHT_SEMI_BOLD, 14.5 * scale),
            make_format(&self.dwrite, DWRITE_FONT_WEIGHT_REGULAR, 12.0 * scale),
            make_format(&self.dwrite, DWRITE_FONT_WEIGHT_REGULAR, 11.5 * scale),
            make_format_wrapped(
                &self.dwrite,
                DWRITE_FONT_WEIGHT_BOLD,
                LYRIC_BOLD_SIZE * scale,
                true,
            ),
            make_format(&self.dwrite, DWRITE_FONT_WEIGHT_REGULAR, LYRIC_SIZE * scale),
        ) {
            self.title = title;
            self.subtitle = subtitle;
            self.time = time;
            self.lyric_current = lyric_current;
            self.lyric_secondary = lyric_secondary;
            self.title_line = None;
            self.subtitle_line = None;
            self.time_lines = [None, None];
            self.seek_line = None;
            self.lyric_cache.clear();
            self.drawn = None;
            self.scale = scale;
        }
    }

    /// Ancho (píxeles físicos) de la más larga de las dos líneas, como las dibujaría `draw`.
    /// Las maqueta y las guarda: `draw` las reutiliza sin volver a maquetar.
    pub fn measure(&mut self, title: &str, subtitle: &str) -> f32 {
        let t = line_for(&self.dwrite, &self.title, &mut self.title_line, title)
            .map_or(0.0, |l| l.width);
        let s = line_for(
            &self.dwrite,
            &self.subtitle,
            &mut self.subtitle_line,
            subtitle,
        )
        .map_or(0.0, |l| l.width);
        t.max(s)
    }

    /// Dibuja textos encima de lo que ya hay en el DIB `dib` (BGRA premultiplicado de
    /// `width` x `height`), solo dentro de `zone` (x0, y0, x1, y1): fuera no cambia nada.
    /// `t` = segundos desde que empezó el desplazamiento. Si alguna línea no cabe y se
    /// desplaza, devuelve en cuántos ms hay que dibujar el cuadro siguiente: lo que tarda en
    /// avanzar un píxel, o lo que falta para que termine la pausa en un extremo (quieta, no
    /// hace falta redibujarla). `None` si todo cabe.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        dib: &mut [u8],
        width: u32,
        height: u32,
        zone: (u32, u32, u32, u32),
        areas: &LayoutAreas,
        title: &str,
        subtitle: &str,
        t: f32,
    ) -> Option<u32> {
        let (x0, y0, x1, y1) = zone;
        if x0 >= x1 || y0 >= y1 || dib.len() < (width * height * 4) as usize {
            return None;
        }
        if self.canvas_size != (width, height) && self.resize(width, height).is_err() {
            return None;
        }
        // Mismos textos en el mismo lugar que el cuadro anterior (se movió la barra, un botón,
        // las barras de audio...): la imagen ya los tiene, solo se vuelven a mezclar.
        let signature = signature(areas, title, subtitle);
        if self.drawn == Some((signature, zone)) {
            self.blend_into(dib, width, zone);
            return None;
        }
        self.drawn = None;
        let s = self.scale;
        let title_line = line_for(&self.dwrite, &self.title, &mut self.title_line, title);
        let subtitle_line = line_for(
            &self.dwrite,
            &self.subtitle,
            &mut self.subtitle_line,
            subtitle,
        );

        let zone_rect = D2D_RECT_F {
            left: x0 as f32,
            top: y0 as f32,
            right: x1 as f32,
            bottom: y1 as f32,
        };
        // Segundos hasta que alguna línea vuelva a moverse (0 = se está moviendo).
        let mut still: Option<f32> = None;
        let mut track = |next: Option<f32>| {
            if let Some(n) = next {
                still = Some(still.map_or(n, |prev| prev.min(n)));
            }
        };
        unsafe {
            self.target.BeginDraw();
            self.target.SetTransform(&Matrix3x2::identity());
            // Solo la zona que cambió: se borra el texto del cuadro anterior y se dibuja el nuevo.
            self.target
                .PushAxisAlignedClip(&zone_rect, D2D1_ANTIALIAS_MODE_ALIASED);
            self.target.Clear(Some(&D2D1_COLOR_F::default()));

            // 1. Título y artista
            if let Some(line) = title_line {
                track(draw_line(
                    &self.target,
                    &self.brush,
                    line,
                    &areas.text,
                    areas.text.cy - 19.0 * s,
                    20.0 * s,
                    (1.0, 1.0, 1.0),
                    s,
                    t,
                ));
            }
            if let Some(line) = subtitle_line {
                track(draw_line(
                    &self.target,
                    &self.brush,
                    line,
                    &areas.text,
                    areas.text.cy + 1.0 * s,
                    18.0 * s,
                    (0.65, 0.65, 0.68),
                    s,
                    t,
                ));
            }

            // 2. Tiempos actual y restante
            if let Some(time) = &areas.time {
                draw_simple_text(
                    &self.target,
                    &self.dwrite,
                    &self.time,
                    &self.brush,
                    &mut self.time_lines[0],
                    &time.current,
                    time.cur_x,
                    time.y,
                    42.0 * s,
                    16.0 * s,
                    (0.92, 0.92, 0.92, time.alpha),
                );
                draw_simple_text(
                    &self.target,
                    &self.dwrite,
                    &self.time,
                    &self.brush,
                    &mut self.time_lines[1],
                    &time.remaining,
                    time.rem_x,
                    time.y,
                    44.0 * s,
                    16.0 * s,
                    (0.65, 0.65, 0.68, time.alpha),
                );
            }

            // 3. Letra sincronizada a la derecha: un carrusel que sube un verso cada vez que
            // empieza el siguiente.
            if let Some(lyrics) = &areas.lyrics {
                let lx = lyrics.x;
                let lw = lyrics.w;
                let cy = lyrics.cy;
                let a = lyrics.alpha;

                if lw > 1.0 && a > 0.001 {
                    let clip = D2D_RECT_F {
                        left: lx,
                        top: cy - 70.0 * s,
                        right: lx + lw,
                        bottom: cy + 70.0 * s,
                    };
                    self.target
                        .PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);

                    if let Some(status) = &lyrics.status {
                        if let Some(line) = cached_line(
                            &self.dwrite,
                            &self.lyric_secondary,
                            &mut self.lyric_cache,
                            false,
                            status,
                        ) {
                            draw_lyric(
                                &self.target,
                                &self.brush,
                                line,
                                (lx, cy - 10.0 * s, lw, 24.0 * s),
                                1.0,
                                (0.55, 0.55, 0.60, a * 0.8),
                                None,
                            );
                        }
                    } else {
                        for (offset, text) in &lyrics.lines {
                            let pos = *offset as f32 + lyrics.shift;
                            let Some((top, alpha, size)) = lyric_slot(pos) else {
                                continue;
                            };
                            if text.is_empty() {
                                // Pausa sin letra: una nota musical para que no quede vacío.
                                draw_note(
                                    &self.target,
                                    &self.brush,
                                    (lx, cy + top * s),
                                    s * size / LYRIC_SIZE,
                                    a * alpha,
                                );
                                continue;
                            }
                            // Cerca del centro el verso pasa de normal a negrita fundiendo
                            // ambos, así el cambio de peso no salta.
                            let bold = (1.0 - pos.abs()).clamp(0.0, 1.0);
                            // El que se canta: lo cantado en blanco y el resto tenue. Al
                            // llegar al centro va bajando hacia el tenue, así no parpadea.
                            let sung = lyrics.sung.filter(|_| *offset == 0);
                            let base = match sung {
                                Some(_) => alpha + (UNSUNG_ALPHA - alpha) * bold,
                                None => alpha,
                            };
                            for (is_bold, weight) in [(true, bold), (false, 1.0 - bold)] {
                                let full = a * alpha * weight;
                                if full <= 0.001 {
                                    continue;
                                }
                                let (format, natural, h) = if is_bold {
                                    (&self.lyric_current, LYRIC_BOLD_SIZE, 44.0)
                                } else {
                                    (&self.lyric_secondary, LYRIC_SIZE, 20.0)
                                };
                                if let Some(line) = cached_line(
                                    &self.dwrite,
                                    format,
                                    &mut self.lyric_cache,
                                    is_bold,
                                    text,
                                ) {
                                    draw_lyric(
                                        &self.target,
                                        &self.brush,
                                        line,
                                        (lx, cy + top * s, lw, h * s),
                                        size / natural,
                                        (1.0, 1.0, 1.0, a * base * weight),
                                        sung.map(|p| (p, full)),
                                    );
                                }
                            }
                        }
                    }

                    self.target.PopAxisAlignedClip();
                }
            }

            // 4. Globo con el tiempo al que se saltaría, encima de todo.
            if let Some(tip) = &areas.seek {
                if let Some(line) =
                    line_for(&self.dwrite, &self.time, &mut self.seek_line, &tip.text)
                {
                    let (w, h) = (line.width + 14.0 * s, 20.0 * s);
                    let x = (tip.x - w / 2.0).clamp(tip.min_x, (tip.max_x - w).max(tip.min_x));
                    let y = tip.bar_cy - 12.0 * s - h;
                    let rect = D2D_RECT_F {
                        left: x,
                        top: y,
                        right: x + w,
                        bottom: y + h,
                    };
                    // El gris de los menús: se distingue sobre el negro de la isla.
                    self.brush.SetColor(&D2D1_COLOR_F {
                        r: 44.0 / 255.0,
                        g: 44.0 / 255.0,
                        b: 48.0 / 255.0,
                        a: tip.alpha,
                    });
                    self.target.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect,
                            radiusX: 6.0 * s,
                            radiusY: 6.0 * s,
                        },
                        &self.brush,
                    );
                    // Centrado en el globo, a lo ancho y a lo alto.
                    let _ = line.layout.SetMaxWidth(w);
                    let _ = line.layout.SetMaxHeight(h);
                    let _ = line.layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                    let _ = line
                        .layout
                        .SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                    self.brush.SetColor(&D2D1_COLOR_F {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: tip.alpha,
                    });
                    self.target.DrawTextLayout(
                        Vector2 { X: x, Y: y },
                        &line.layout,
                        &self.brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
            }

            self.target.PopAxisAlignedClip();
            if self.target.EndDraw(None, None).is_err() {
                // El destino se perdió (pasa muy rara vez): se vuelve a crear en el próximo cuadro.
                self.canvas_size = (0, 0);
                return still.map(|_| SCROLL_FRAME_MS.0);
            }
        }
        // Un texto que se desplaza depende de `t`: no se reutiliza.
        if still.is_none() {
            self.drawn = Some((signature, zone));
        }
        self.blend_into(dib, width, zone);
        still.map(|secs| {
            if secs <= 0.0 {
                // Un cuadro por píxel físico de avance: igual de suave y sin cuadros repetidos.
                let ms = 1000.0 / (SCROLL_SPEED * s);
                (ms as u32).clamp(SCROLL_FRAME_MS.0, SCROLL_FRAME_MS.1)
            } else {
                (secs * 1000.0).ceil() as u32
            }
        })
    }

    /// Crea la imagen del texto (y su destino de dibujo) del tamaño de la ventana.
    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        unsafe {
            let canvas = self.wic.CreateBitmap(
                width,
                height,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapCacheOnDemand,
            )?;
            let (target, brush) = make_target(&self.d2d, &canvas)?;
            self.canvas = Some(canvas);
            self.target = target;
            self.brush = brush;
            self.canvas_size = (width, height);
            self.drawn = None;
        }
        Ok(())
    }

    /// Mezcla el texto dibujado en `zone` sobre el DIB: texto encima de lo que hay (ambos con
    /// alfa premultiplicado). Donde no hay texto (alfa 0) el DIB queda igual.
    fn blend_into(&self, dib: &mut [u8], width: u32, (x0, y0, x1, y1): (u32, u32, u32, u32)) {
        let Some(canvas) = &self.canvas else { return };
        let rect = WICRect {
            X: x0 as i32,
            Y: y0 as i32,
            Width: (x1 - x0) as i32,
            Height: (y1 - y0) as i32,
        };
        unsafe {
            let Ok(lock) = canvas.Lock(&rect, WICBitmapLockRead.0 as u32) else {
                return;
            };
            let (Ok(stride), mut size, mut data) = (lock.GetStride(), 0u32, std::ptr::null_mut())
            else {
                return;
            };
            if lock.GetDataPointer(&mut size, &mut data).is_err() || data.is_null() {
                return;
            }
            let src = std::slice::from_raw_parts(data, size as usize);
            let row_bytes = (x1 - x0) as usize * 4;
            let dib_stride = width as usize * 4;
            for row in 0..(y1 - y0) as usize {
                let s = &src[row * stride as usize..][..row_bytes];
                let at = (y0 as usize + row) * dib_stride + x0 as usize * 4;
                let d = &mut dib[at..at + row_bytes];
                for (d, s) in d
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(s.as_chunks::<4>().0)
                {
                    let a = s[3] as u32;
                    if a == 0 {
                        continue;
                    }
                    let keep = 255 - a;
                    for c in 0..4 {
                        d[c] = (s[c] as u32 + (d[c] as u32 * keep + 127) / 255) as u8;
                    }
                }
            }
        }
    }
}

/// Resumen de todo lo que decide cómo se ven los textos de un cuadro: si coincide con el del
/// cuadro anterior, el dibujo sería idéntico.
fn signature(areas: &LayoutAreas, title: &str, subtitle: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut f = |v: f32| h.write_u32(v.to_bits());
    let a = &areas.text;
    [a.x, a.w, a.cy, a.alpha].into_iter().for_each(&mut f);
    if let Some(t) = &areas.time {
        [t.cur_x, t.rem_x, t.y, t.alpha]
            .into_iter()
            .for_each(&mut f);
    }
    if let Some(l) = &areas.lyrics {
        [l.x, l.w, l.cy, l.shift, l.alpha, l.sung.unwrap_or(-1.0)]
            .into_iter()
            .for_each(&mut f);
    }
    if let Some(s) = &areas.seek {
        [s.x, s.bar_cy, s.min_x, s.max_x, s.alpha]
            .into_iter()
            .for_each(&mut f);
    }
    (title, subtitle).hash(&mut h);
    areas
        .time
        .as_ref()
        .map(|t| (&t.current, &t.remaining))
        .hash(&mut h);
    areas.seek.as_ref().map(|s| &s.text).hash(&mut h);
    areas
        .lyrics
        .as_ref()
        .map(|l| (&l.lines, &l.status))
        .hash(&mut h);
    h.finish()
}

/// Destino de Direct2D (en software) que dibuja sobre `canvas`, con el pincel de los textos.
fn make_target(
    d2d: &ID2D1Factory,
    canvas: &IWICBitmap,
) -> Result<(ID2D1RenderTarget, ID2D1SolidColorBrush)> {
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        // 96 = trabajamos en píxeles físicos; el escalado lo aplicamos nosotros.
        dpiX: 96.0,
        dpiY: 96.0,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    };
    unsafe {
        let target = d2d.CreateWicBitmapRenderTarget(canvas, &props)?;
        // ClearType necesita fondo opaco conocido; la escala de grises sirve sobre la isla.
        target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let brush = target.CreateSolidColorBrush(
            &D2D1_COLOR_F {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            None,
        )?;
        Ok((target, brush))
    }
}

/// Dibuja una etiqueta reutilizando su layout y el pincel del renderer.
/// El texto se maqueta al cambiar; los límites se actualizan sin crear objetos COM.
#[allow(clippy::too_many_arguments)]
fn draw_simple_text(
    target: &ID2D1RenderTarget,
    dwrite: &IDWriteFactory,
    format: &IDWriteTextFormat,
    brush: &ID2D1SolidColorBrush,
    cache: &mut Option<Line>,
    text: &str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    (r, g, b, a): (f32, f32, f32, f32),
) {
    if text.is_empty() || a <= 0.0 {
        return;
    }
    if let Some(line) = line_for(dwrite, format, cache, text) {
        unsafe {
            if line.layout.GetMaxWidth() != w {
                let _ = line.layout.SetMaxWidth(w);
            }
            if line.layout.GetMaxHeight() != h {
                let _ = line.layout.SetMaxHeight(h);
            }
            brush.SetColor(&D2D1_COLOR_F { r, g, b, a });
            target.DrawTextLayout(
                Vector2 { X: x, Y: y },
                &line.layout,
                brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
}

/// Lugar del carrusel para un verso a `pos` versos del que se canta (con decimales mientras
/// sube): (borde superior, opacidad, tamaño de letra). `None` fuera de la columna.
fn lyric_slot(pos: f32) -> Option<(f32, f32, f32)> {
    let p = pos + 2.0;
    if !(0.0..=5.0).contains(&p) {
        return None;
    }
    let i = (p.floor() as usize).min(LYRIC_SLOTS.len() - 2);
    let f = p - i as f32;
    let (a, b) = (LYRIC_SLOTS[i], LYRIC_SLOTS[i + 1]);
    let lerp = |x: f32, y: f32| x + (y - x) * f;
    Some((lerp(a.0, b.0), lerp(a.1, b.1), lerp(a.2, b.2)))
}

/// Verso maquetado en negrita o normal. Al subir el carrusel los textos solo cambian de lugar,
/// así que casi siempre ya está guardado; si no, se maqueta y se olvida el más viejo.
fn cached_line<'a>(
    dwrite: &IDWriteFactory,
    format: &IDWriteTextFormat,
    cache: &'a mut Vec<(bool, Line)>,
    bold: bool,
    text: &str,
) -> Option<&'a Line> {
    let i = match cache.iter().position(|(b, l)| *b == bold && l.text == text) {
        Some(i) => i,
        None => {
            let mut fresh = None;
            line_for(dwrite, format, &mut fresh, text)?;
            if cache.len() >= LYRIC_CACHE {
                cache.remove(0);
            }
            cache.push((bold, fresh?));
            cache.len() - 1
        }
    };
    cache.get(i).map(|(_, l)| l)
}

/// Dibuja un verso en su caja (x, y, ancho, alto), escalado `zoom` desde la esquina superior
/// izquierda: el tamaño cambia de forma continua sin volver a maquetar en cada cuadro.
/// `sung`: (hasta dónde va cantado, opacidad de esa parte), que se vuelve a dibujar encima
/// recortada a lo cantado.
fn draw_lyric(
    target: &ID2D1RenderTarget,
    brush: &ID2D1SolidColorBrush,
    line: &Line,
    (x, y, w, h): (f32, f32, f32, f32),
    zoom: f32,
    (r, g, b, a): (f32, f32, f32, f32),
    sung: Option<(f32, f32)>,
) {
    unsafe {
        if line.layout.GetMaxWidth() != w {
            let _ = line.layout.SetMaxWidth(w);
        }
        if line.layout.GetMaxHeight() != h {
            let _ = line.layout.SetMaxHeight(h);
        }
        brush.SetColor(&D2D1_COLOR_F { r, g, b, a });
        target.SetTransform(&Matrix3x2 {
            M11: zoom,
            M12: 0.0,
            M21: 0.0,
            M22: zoom,
            M31: x * (1.0 - zoom),
            M32: y * (1.0 - zoom),
        });
        target.DrawTextLayout(
            Vector2 { X: x, Y: y },
            &line.layout,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
        if let Some((pos, alpha)) = sung.filter(|&(pos, _)| pos > 0.0) {
            brush.SetColor(&D2D1_COLOR_F { r, g, b, a: alpha });
            // Los recortes van con la misma escala que el texto (la transformación sigue puesta).
            for rect in sung_rects(&line.layout, pos, w) {
                if rect.right <= rect.left || rect.bottom <= rect.top {
                    continue;
                }
                let clip = D2D_RECT_F {
                    left: x + rect.left,
                    top: y + rect.top,
                    right: x + rect.right,
                    bottom: y + rect.bottom,
                };
                target.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
                target.DrawTextLayout(
                    Vector2 { X: x, Y: y },
                    &line.layout,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
                target.PopAxisAlignedClip();
            }
        }
        target.SetTransform(&Matrix3x2::identity());
    }
}

/// Dos corcheas unidas (ícono de nota musical) con la esquina superior izquierda en (x, y);
/// `k` = píxeles físicos por unidad lógica a ese tamaño. Ocupa unos 18 x 16.
fn draw_note(
    target: &ID2D1RenderTarget,
    brush: &ID2D1SolidColorBrush,
    (x, y): (f32, f32),
    k: f32,
    alpha: f32,
) {
    if alpha <= 0.001 {
        return;
    }
    let rect = |l: f32, t: f32, r: f32, b: f32| D2D_RECT_F {
        left: x + l * k,
        top: y + t * k,
        right: x + r * k,
        bottom: y + b * k,
    };
    unsafe {
        brush.SetColor(&D2D1_COLOR_F {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: alpha,
        });
        // Barra que las une y las dos plicas.
        target.FillRectangle(&rect(6.6, 2.0, 18.0, 4.6), brush);
        target.FillRectangle(&rect(6.6, 2.0, 8.2, 14.0), brush);
        target.FillRectangle(&rect(16.4, 2.0, 18.0, 14.0), brush);
        // Cabezas: óvalos al pie de cada plica.
        for cx in [4.6, 14.4] {
            target.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 {
                        X: x + cx * k,
                        Y: y + 14.2 * k,
                    },
                    radiusX: 3.6 * k,
                    radiusY: 2.7 * k,
                },
                brush,
            );
        }
    }
}

/// Parte cantada de un verso maquetado, relativa a su esquina: los renglones de arriba
/// completos (si el verso ocupa dos) y el renglón actual hasta la posición `pos`, que cae
/// entre dos letras con decimales para que el borde avance suave.
pub(crate) fn sung_rects(layout: &IDWriteTextLayout, pos: f32, w: f32) -> [D2D_RECT_F; 2] {
    let k = pos.floor().max(0.0);
    let mut metrics = DWRITE_HIT_TEST_METRICS::default();
    let (mut px, mut py) = (0.0, 0.0);
    unsafe {
        let _ = layout.HitTestTextPosition(k as u32, false, &mut px, &mut py, &mut metrics);
    }
    let edge = metrics.left + metrics.width * (pos - k);
    [
        D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: w,
            bottom: metrics.top,
        },
        D2D_RECT_F {
            left: 0.0,
            top: metrics.top,
            right: edge,
            bottom: metrics.top + metrics.height,
        },
    ]
}

/// Devuelve la línea maquetada de `text`, creándola solo si el texto cambió.
fn line_for<'a>(
    dwrite: &IDWriteFactory,
    format: &IDWriteTextFormat,
    cache: &'a mut Option<Line>,
    text: &str,
) -> Option<&'a Line> {
    if cache.as_ref().is_none_or(|l| l.text != text) {
        *cache = unsafe {
            let wide: Vec<u16> = text.encode_utf16().collect();
            dwrite
                .CreateTextLayout(&wide, format, f32::MAX, f32::MAX)
                .ok()
                .and_then(|layout| {
                    let mut metrics = DWRITE_TEXT_METRICS::default();
                    layout.GetMetrics(&mut metrics).ok()?;
                    Some(Line {
                        text: text.to_string(),
                        layout,
                        width: metrics.width,
                    })
                })
        };
    }
    cache.as_ref()
}

/// Dibuja una línea recortada a su columna. Si no cabe, devuelve los segundos que seguirá
/// quieta (0 = se está desplazando); `None` si cabe.
#[allow(clippy::too_many_arguments)]
fn draw_line(
    target: &ID2D1RenderTarget,
    brush: &ID2D1SolidColorBrush,
    line: &Line,
    area: &TextArea,
    y: f32,
    h: f32,
    (r, g, b): (f32, f32, f32),
    scale: f32,
    t: f32,
) -> Option<f32> {
    let overflow = line.width - area.w;
    let (offset, still) = if overflow > 0.0 {
        let (offset, still) = scroll_offset(overflow, SCROLL_SPEED * scale, t);
        (offset, Some(still))
    } else {
        (0.0, None)
    };
    let color = D2D1_COLOR_F {
        r,
        g,
        b,
        a: area.alpha,
    };
    let clip = D2D_RECT_F {
        left: area.x,
        top: y,
        right: area.x + area.w,
        bottom: y + h,
    };
    unsafe {
        brush.SetColor(&color);
        target.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
        target.DrawTextLayout(
            Vector2 {
                X: area.x - offset,
                Y: y,
            },
            &line.layout,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
        target.PopAxisAlignedClip();
    }
    still
}

/// Ida y vuelta: pausa, avanza `overflow` píxeles, pausa, regresa. Se repite.
/// Devuelve el desplazamiento y cuántos segundos faltan para que termine la pausa actual
/// (0 mientras avanza o regresa).
fn scroll_offset(overflow: f32, speed: f32, t: f32) -> (f32, f32) {
    let travel = overflow / speed;
    let cycle = 2.0 * (SCROLL_PAUSE + travel);
    let phase = t % cycle;
    if phase < SCROLL_PAUSE {
        (0.0, SCROLL_PAUSE - phase)
    } else if phase < SCROLL_PAUSE + travel {
        ((phase - SCROLL_PAUSE) * speed, 0.0)
    } else if phase < 2.0 * SCROLL_PAUSE + travel {
        (overflow, 2.0 * SCROLL_PAUSE + travel - phase)
    } else {
        let back = phase - 2.0 * SCROLL_PAUSE - travel;
        (overflow - back * speed, 0.0)
    }
}

/// Formato de una línea: sin saltos (el desplazamiento muestra lo que no cabe).
pub(crate) fn make_format(
    dwrite: &IDWriteFactory,
    weight: DWRITE_FONT_WEIGHT,
    size: f32,
) -> Result<IDWriteTextFormat> {
    make_format_wrapped(dwrite, weight, size, false)
}

/// Formato con ajuste de línea opcional: el verso actual puede ocupar dos renglones.
pub(crate) fn make_format_wrapped(
    dwrite: &IDWriteFactory,
    weight: DWRITE_FONT_WEIGHT,
    size: f32,
    wrap: bool,
) -> Result<IDWriteTextFormat> {
    unsafe {
        // Si a Segoe UI le falta un carácter, DirectWrite elige otra fuente automáticamente.
        let format = dwrite.CreateTextFormat(
            w!("Segoe UI"),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!(""),
        )?;
        let wrapping = if wrap {
            DWRITE_WORD_WRAPPING_WRAP
        } else {
            DWRITE_WORD_WRAPPING_NO_WRAP
        };
        format.SetWordWrapping(wrapping)?;
        Ok(format)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carrusel_interpola_entre_lugares() {
        // En un lugar exacto, sus valores; a mitad de camino, el promedio.
        assert_eq!(lyric_slot(0.0), Some(LYRIC_SLOTS[2]));
        assert_eq!(lyric_slot(1.0), Some(LYRIC_SLOTS[3]));
        let (top, alpha, size) = lyric_slot(0.5).unwrap();
        assert!((top - 2.0).abs() < 1e-4);
        assert!((alpha - 0.675).abs() < 1e-4);
        assert!((size - 14.0).abs() < 1e-4);
        // Fuera de la columna no se dibuja.
        assert_eq!(lyric_slot(-2.5), None);
        assert_eq!(lyric_slot(5.5), None);
    }

    #[test]
    fn la_memoria_de_versos_no_crece() {
        // Una canción entera pasando por el carrusel: se guardan a lo sumo `LYRIC_CACHE`.
        let dwrite: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.expect("DirectWrite");
        let format = make_format(&dwrite, DWRITE_FONT_WEIGHT_REGULAR, 12.5).unwrap();
        let mut cache = Vec::new();
        for i in 0..200 {
            let text = format!("verso numero {i}");
            assert!(cached_line(&dwrite, &format, &mut cache, i % 2 == 0, &text).is_some());
        }
        assert_eq!(cache.len(), LYRIC_CACHE);
        // Un verso ya guardado no se vuelve a maquetar.
        let before = cache.len();
        cached_line(&dwrite, &format, &mut cache, false, "verso numero 199");
        assert_eq!(cache.len(), before);
    }
}
