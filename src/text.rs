//! Texto con DirectWrite, dibujado por Direct2D directamente sobre el DIB de la ventana.
//!
//! Se usa el modo software de Direct2D: no carga drivers de GPU (menos RAM) y para unas
//! pocas líneas de texto es igual de rápido.
//!
//! Si una línea no cabe en su columna se desplaza de un lado al otro (marquee): pausa al
//! inicio, avanza hasta mostrar el final, pausa, y vuelve.
//! Además maquetamos y dibujamos las etiquetas de tiempo y la letra sincronizada a la derecha.

use windows::core::{w, Result};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_ALIASED,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_REGULAR,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP,
    DWRITE_WORD_WRAPPING_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::HDC;
use windows_numerics::Vector2;

use crate::island::{LayoutAreas, TextArea};

/// Velocidad lógica del desplazamiento, en píxeles por segundo.
const SCROLL_SPEED: f32 = 30.0;
/// Pausa en cada extremo, en segundos.
const SCROLL_PAUSE: f32 = 1.5;
/// Límites del intervalo entre cuadros mientras el texto avanza (~60 y ~30 fps).
const SCROLL_FRAME_MS: (u32, u32) = (15, 33);

/// Una línea ya maquetada, guardada mientras el texto no cambie.
struct Line {
    text: String,
    layout: IDWriteTextLayout,
    width: f32,
}

/// Formatos, layouts y pincel reutilizables; se invalidan los textos al cambiar el DPI.
pub struct TextRenderer {
    dwrite: IDWriteFactory,
    target: ID2D1DCRenderTarget,
    title: IDWriteTextFormat,
    subtitle: IDWriteTextFormat,
    time: IDWriteTextFormat,
    lyric_current: IDWriteTextFormat,
    lyric_secondary: IDWriteTextFormat,
    lyric_faint: IDWriteTextFormat,
    title_line: Option<Line>,
    subtitle_line: Option<Line>,
    // Una entrada por etiqueta: memoria acotada aunque cambien canción y tiempos.
    time_lines: [Option<Line>; 2],
    lyric_lines: [Option<Line>; 5],
    brush: ID2D1SolidColorBrush,
    scale: f32,
}

impl TextRenderer {
    /// Crea los formatos y un pincel compartido para todos los textos de la isla.
    pub fn new(scale: f32) -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
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
            let target = factory.CreateDCRenderTarget(&props)?;
            // ClearType necesita fondo opaco conocido; la escala de grises sirve sobre la isla.
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);

            let brush = target.CreateSolidColorBrush(
                &D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
                None,
            )?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let title = make_format(&dwrite, DWRITE_FONT_WEIGHT_SEMI_BOLD, 14.5 * scale)?;
            let subtitle = make_format(&dwrite, DWRITE_FONT_WEIGHT_REGULAR, 12.0 * scale)?;
            let time = make_format(&dwrite, DWRITE_FONT_WEIGHT_REGULAR, 11.5 * scale)?;
            let lyric_current =
                make_format_wrapped(&dwrite, DWRITE_FONT_WEIGHT_BOLD, 15.5 * scale, true)?;
            let lyric_secondary =
                make_format(&dwrite, DWRITE_FONT_WEIGHT_REGULAR, 12.5 * scale)?;
            let lyric_faint = make_format(&dwrite, DWRITE_FONT_WEIGHT_REGULAR, 11.0 * scale)?;

            Ok(Self {
                dwrite,
                target,
                title,
                subtitle,
                time,
                lyric_current,
                lyric_secondary,
                lyric_faint,
                brush,
                time_lines: [None, None],
                lyric_lines: [None, None, None, None, None],
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
        if let (Ok(title), Ok(subtitle), Ok(time), Ok(lyric_current), Ok(lyric_secondary), Ok(lyric_faint)) = (
            make_format(&self.dwrite, DWRITE_FONT_WEIGHT_SEMI_BOLD, 14.5 * scale),
            make_format(&self.dwrite, DWRITE_FONT_WEIGHT_REGULAR, 12.0 * scale),
            make_format(&self.dwrite, DWRITE_FONT_WEIGHT_REGULAR, 11.5 * scale),
            make_format_wrapped(&self.dwrite, DWRITE_FONT_WEIGHT_BOLD, 15.5 * scale, true),
            make_format(&self.dwrite, DWRITE_FONT_WEIGHT_REGULAR, 12.5 * scale),
            make_format(&self.dwrite, DWRITE_FONT_WEIGHT_REGULAR, 11.0 * scale),
        ) {
            self.title = title;
            self.subtitle = subtitle;
            self.time = time;
            self.lyric_current = lyric_current;
            self.lyric_secondary = lyric_secondary;
            self.lyric_faint = lyric_faint;
            self.title_line = None;
            self.subtitle_line = None;
            self.time_lines = [None, None];
            self.lyric_lines = [None, None, None, None, None];
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

    /// Dibuja textos encima de lo que ya hay en el DIB seleccionado en `dc`.
    /// `t` = segundos desde que empezó el desplazamiento. Si alguna línea no cabe y se
    /// desplaza, devuelve en cuántos ms hay que dibujar el cuadro siguiente: lo que tarda en
    /// avanzar un píxel, o lo que falta para que termine la pausa en un extremo (quieta, no
    /// hace falta redibujarla). `None` si todo cabe.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        dc: HDC,
        width: u32,
        height: u32,
        areas: &LayoutAreas,
        title: &str,
        subtitle: &str,
        t: f32,
    ) -> Option<u32> {
        let s = self.scale;
        let title_line = line_for(&self.dwrite, &self.title, &mut self.title_line, title);
        let subtitle_line = line_for(
            &self.dwrite,
            &self.subtitle,
            &mut self.subtitle_line,
            subtitle,
        );

        let full = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        // Segundos hasta que alguna línea vuelva a moverse (0 = se está moviendo).
        let mut still: Option<f32> = None;
        let mut track = |next: Option<f32>| {
            if let Some(n) = next {
                still = Some(still.map_or(n, |prev| prev.min(n)));
            }
        };
        unsafe {
            if self.target.BindDC(dc, &full).is_err() {
                return None;
            }
            self.target.BeginDraw();

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

            // 3. Letras sincronizadas a la derecha
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
                    self.target.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);

                    if let Some(status) = &lyrics.status {
                        draw_simple_text(
                            &self.target,
                            &self.dwrite,
                            &self.lyric_secondary,
                            &self.brush,
                            &mut self.lyric_lines[0],
                            status,
                            lx,
                            cy - 10.0 * s,
                            lw,
                            24.0 * s,
                            (0.55, 0.55, 0.60, a * 0.8),
                        );
                    } else {
                        if let Some(prev) = &lyrics.prev {
                            draw_simple_text(
                                &self.target,
                                &self.dwrite,
                                &self.lyric_secondary,
                                &self.brush,
                                &mut self.lyric_lines[1],
                                prev,
                                lx,
                                cy - 44.0 * s,
                                lw,
                                20.0 * s,
                                (1.0, 1.0, 1.0, a * 0.40),
                            );
                        }
                        if let Some(curr) = &lyrics.current {
                            draw_simple_text(
                                &self.target,
                                &self.dwrite,
                                &self.lyric_current,
                                &self.brush,
                                &mut self.lyric_lines[2],
                                curr,
                                lx,
                                cy - 20.0 * s,
                                lw,
                                44.0 * s,
                                (1.0, 1.0, 1.0, a),
                            );
                        }
                        if let Some(next) = &lyrics.next {
                            draw_simple_text(
                                &self.target,
                                &self.dwrite,
                                &self.lyric_secondary,
                                &self.brush,
                                &mut self.lyric_lines[3],
                                next,
                                lx,
                                cy + 24.0 * s,
                                lw,
                                20.0 * s,
                                (1.0, 1.0, 1.0, a * 0.35),
                            );
                        }
                        if let Some(next2) = &lyrics.next2 {
                            draw_simple_text(
                                &self.target,
                                &self.dwrite,
                                &self.lyric_faint,
                                &self.brush,
                                &mut self.lyric_lines[4],
                                next2,
                                lx,
                                cy + 45.0 * s,
                                lw,
                                18.0 * s,
                                (1.0, 1.0, 1.0, a * 0.20),
                            );
                        }
                    }

                    self.target.PopAxisAlignedClip();
                }
            }

            let _ = self.target.EndDraw(None, None);
        }
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
}

/// Dibuja una etiqueta reutilizando su layout y el pincel del renderer.
/// El texto se maqueta al cambiar; los límites se actualizan sin crear objetos COM.
#[allow(clippy::too_many_arguments)]
fn draw_simple_text(
    target: &ID2D1DCRenderTarget,
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
                Vector2 { X: x, Y: y }, &line.layout, brush, D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
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
    target: &ID2D1DCRenderTarget,
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
