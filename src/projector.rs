//! Proyector de la letra: una píldora negra, como la isla, justo debajo de ella, con el verso
//! que se canta. Cada verso nuevo aparece desvaneciéndose y la píldora se ajusta a su largo
//! con el mismo resorte de la isla. Al empezar o terminar la letra baja o sube desvaneciéndose.
//!
//! Es una ventana aparte, layered y transparente al mouse (`WS_EX_TRANSPARENT`): los clics la
//! atraviesan, así que nunca tapa lo que hay debajo.

use std::time::Instant;

use tiny_skia::{Color, Paint};
use windows::core::{w, Result};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
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
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, ShowWindow,
    UpdateLayeredWindow, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::island::{fill_rounded_rect, rounded_rect_path, stroke_path};
use crate::spring::Spring;
use crate::text::{make_format, sung_rects, UNSUNG_ALPHA};
use crate::Surface;

// Medidas en píxeles lógicos.
/// Ancho máximo del texto; un verso más largo pasa a una segunda línea.
const MAX_TEXT_W: f32 = 420.0;
const MIN_W: f32 = 80.0;
const PAD_X: f32 = 16.0;
const PAD_Y: f32 = 9.0;
/// Alto de la ventana: caben dos líneas de texto, los márgenes y la bajada al aparecer.
const WINDOW_H: f32 = 72.0;
/// Margen a los lados para que el rebote del resorte no se recorte.
const MARGIN: f32 = 12.0;
const FONT: f32 = 14.0;
/// Cuánto baja al aparecer (sale de detrás de la isla).
const DROP: f32 = 8.0;
/// Cuánto tarda en aparecer cada verso.
const FADE_SECS: f32 = 0.25;

/// Verso maquetado (medidas en píxeles físicos).
struct Verse {
    text: String,
    layout: IDWriteTextLayout,
    w: f32,
    h: f32,
}

pub struct Projector {
    hwnd: HWND,
    surface: Surface,
    target: ID2D1DCRenderTarget,
    dwrite: IDWriteFactory,
    format: IDWriteTextFormat,
    scale: f32,
    /// Verso mostrado (se mantiene mientras se desvanece al terminar la letra).
    verse: Option<Verse>,
    since: Instant,
    /// 0 = escondido detrás de la isla, 1 = a la vista.
    presence: Spring,
    /// Tamaño de la píldora (píxeles físicos), con resorte para ajustarse a cada verso.
    width: Spring,
    height: Spring,
    shown: bool,
    last_frame: Instant,
    backdrop: Option<crate::backdrop::Backdrop>,
    blur_radius: u32,
}

impl Projector {
    pub fn new(scale: f32) -> Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = w!("NotchIslaLetra");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(passthrough_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE,
                class,
                w!("Letra de Notch Isla"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )?;

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
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let format = verse_format(&dwrite, scale)?;
            let (w, h) = window_size(scale);
            Ok(Self {
                hwnd,
                surface: Surface::new(w, h)?,
                target,
                dwrite,
                format,
                scale,
                verse: None,
                since: Instant::now(),
                presence: Spring::new(0.0, 260.0, 22.0),
                width: Spring::new(0.0, 260.0, 24.0),
                height: Spring::new(0.0, 260.0, 24.0),
                shown: false,
                last_frame: Instant::now(),
                backdrop: crate::backdrop::Backdrop::new().ok(),
                blur_radius: 16,
            })
        }
    }

    /// Oculta de inmediato, incluso si se detiene el temporizador por pantalla completa.
    pub fn hide(&mut self) {
        if let Some(backdrop) = self.backdrop.as_mut() {
            backdrop.hide();
        }
        if self.shown {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.shown = false;
        }
        self.presence.reset(0.0);
        self.verse = None;
        self.last_frame = Instant::now();
    }

    /// Cambió el DPI: letra y ventana se rehacen a la nueva escala.
    pub fn set_scale(&mut self, scale: f32) {
        if scale == self.scale {
            return;
        }
        let (w, h) = window_size(scale);
        if let (Ok(format), Ok(surface)) = (verse_format(&self.dwrite, scale), Surface::new(w, h)) {
            self.format = format;
            self.surface = surface;
            self.scale = scale;
            // El verso se vuelve a maquetar con la letra nueva en el siguiente cuadro.
            self.verse = None;
        }
    }

    /// Dibuja el proyector con el verso `text` (o lo esconde con `None`), centrado en `cx` y
    /// con su borde superior en `top` (píxeles de pantalla). `sung`: posición cantada para resaltar.
    /// Devuelve `true` mientras se mueve (resorte o desvanecido): hay que seguir dibujando cuadros.
    pub fn render(
        &mut self,
        text: Option<&str>,
        sung: Option<f32>,
        cx: i32,
        top: i32,
        translucent: bool,
        blur_radius: u32,
    ) -> bool {
        self.blur_radius = blur_radius;
        let dt = self.last_frame.elapsed().as_secs_f32().min(0.05);
        self.last_frame = Instant::now();
        let s = self.scale;

        if let Some(text) = text {
            if self.verse.as_ref().is_none_or(|v| v.text != text) {
                let appearing = self.presence.value < 0.05;
                if let Some(verse) = self.layout(text) {
                    let w = (verse.w + 2.0 * PAD_X * s).max(MIN_W * s);
                    let h = verse.h + 2.0 * PAD_Y * s;
                    if appearing {
                        // Al aparecer ya tiene su tamaño: solo baja y se desvanece.
                        self.width.reset(w);
                        self.height.reset(h);
                    } else {
                        self.width.set_target(w);
                        self.height.set_target(h);
                    }
                    self.verse = Some(verse);
                    self.since = Instant::now();
                }
            }
        }
        self.presence
            .set_target(if text.is_some() { 1.0 } else { 0.0 });
        let mut moving = self.presence.step(dt);
        moving |= self.width.step(dt);
        moving |= self.height.step(dt);
        let fade = (self.since.elapsed().as_secs_f32() / FADE_SECS).min(1.0);
        moving |= fade < 1.0;

        let visible = self.presence.value > 0.01;
        if !visible {
            if let Some(backdrop) = self.backdrop.as_mut() {
                backdrop.hide();
            }
            if self.shown {
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
                self.shown = false;
            }
            return moving;
        }
        self.draw(fade, sung, cx, top, translucent);
        if !self.shown {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            self.shown = true;
        }
        if let Some(backdrop) = self.backdrop.as_mut() {
            backdrop.place_behind(self.hwnd);
        }
        moving
    }

    fn layout(&self, text: &str) -> Option<Verse> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let layout = self
                .dwrite
                .CreateTextLayout(&wide, &self.format, MAX_TEXT_W * self.scale, f32::MAX)
                .ok()?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m).ok()?;
            Some(Verse {
                text: text.to_string(),
                layout,
                w: m.width,
                h: m.height,
            })
        }
    }

    fn draw(&mut self, fade: f32, sung: Option<f32>, cx: i32, top: i32, translucent: bool) {
        let s = self.scale;
        let (sw, sh) = (self.surface.width as f32, self.surface.height as f32);
        let w = self.width.value.clamp(1.0, sw - 2.0 * MARGIN * s);
        let h = self.height.value.clamp(1.0, sh - DROP * s);
        let x = (sw - w) / 2.0;
        // Baja desde detrás de la isla mientras aparece.
        let y = DROP * s * self.presence.value.clamp(0.0, 1.0);

        let translucent = if translucent {
            self.backdrop.as_mut().is_some_and(|backdrop| {
                backdrop.show(
                    self.hwnd,
                    (
                        cx - self.surface.width as i32 / 2,
                        top,
                        self.surface.width,
                        self.surface.height,
                    ),
                    (x, y, w, h, (h / 2.0).min(18.0 * s)),
                    (self.blur_radius as f32 * s).round() as u32,
                    self.presence.value,
                )
            })
        } else {
            if let Some(backdrop) = self.backdrop.as_mut() {
                backdrop.hide();
            }
            false
        };

        let pixmap = &mut self.surface.pixmap;
        pixmap.fill(Color::TRANSPARENT);
        let mut paint = Paint {
            anti_alias: true,
            ..Default::default()
        };
        let r = (h / 2.0).min(18.0 * s);
        if translucent {
            paint.set_color_rgba8(20, 20, 24, 100);
            fill_rounded_rect(pixmap, &paint, x, y, w, h, r);
            paint.set_color_rgba8(255, 255, 255, 34);
            if let Some(border) = rounded_rect_path(
                x + 0.5,
                y + 0.5,
                (w - 1.0).max(1.0),
                (h - 1.0).max(1.0),
                (r - 0.5).max(0.0),
            ) {
                stroke_path(pixmap, &paint, &border, 1.0);
            }
        } else {
            paint.set_color_rgba8(0, 0, 0, 255);
            fill_rounded_rect(pixmap, &paint, x, y, w, h, r);
        }
        self.surface.copy_to_dib();

        if let Some(verse) = &self.verse {
            let full = RECT {
                left: 0,
                top: 0,
                right: self.surface.width as i32,
                bottom: self.surface.height as i32,
            };
            unsafe {
                if self.target.BindDC(self.surface.dc, &full).is_ok() {
                    self.target.BeginDraw();
                    let unsung_a = if sung.is_some() {
                        fade * UNSUNG_ALPHA
                    } else {
                        fade
                    };
                    let color = D2D1_COLOR_F {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: unsung_a,
                    };
                    if let Ok(brush) = self.target.CreateSolidColorBrush(&color, None) {
                        // La caja del texto mide `MAX_TEXT_W` y lo centra adentro.
                        let box_w = MAX_TEXT_W * s;
                        let text_x = sw / 2.0 - box_w / 2.0;
                        let text_y = y + (h - verse.h) / 2.0;
                        self.target.DrawTextLayout(
                            windows_numerics::Vector2 {
                                X: text_x,
                                Y: text_y,
                            },
                            &verse.layout,
                            &brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                        if let Some(pos) = sung.filter(|&p| p > 0.0) {
                            brush.SetColor(&D2D1_COLOR_F {
                                r: 1.0,
                                g: 1.0,
                                b: 1.0,
                                a: fade,
                            });
                            for rect in sung_rects(&verse.layout, pos, box_w) {
                                if rect.right <= rect.left || rect.bottom <= rect.top {
                                    continue;
                                }
                                let clip = D2D_RECT_F {
                                    left: text_x + rect.left,
                                    top: text_y + rect.top,
                                    right: text_x + rect.right,
                                    bottom: text_y + rect.bottom,
                                };
                                self.target
                                    .PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
                                self.target.DrawTextLayout(
                                    windows_numerics::Vector2 {
                                        X: text_x,
                                        Y: text_y,
                                    },
                                    &verse.layout,
                                    &brush,
                                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                                );
                                self.target.PopAxisAlignedClip();
                            }
                        }
                    }
                    let _ = self.target.EndDraw(None, None);
                }
            }
        }

        let pos = POINT {
            x: cx - self.surface.width as i32 / 2,
            y: top,
        };
        let size = SIZE {
            cx: self.surface.width as i32,
            cy: self.surface.height as i32,
        };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (self.presence.value.clamp(0.0, 1.0) * 255.0) as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&pos),
                Some(&size),
                Some(self.surface.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }
}

impl Drop for Projector {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Tamaño de la ventana en píxeles físicos.
fn window_size(scale: f32) -> (u32, u32) {
    (
        ((MAX_TEXT_W + 2.0 * (PAD_X + MARGIN)) * scale).ceil() as u32,
        (WINDOW_H * scale).ceil() as u32,
    )
}

/// Letra del verso: semibold, centrada y en varias líneas si no cabe.
fn verse_format(dwrite: &IDWriteFactory, scale: f32) -> Result<IDWriteTextFormat> {
    let format = make_format(dwrite, DWRITE_FONT_WEIGHT_SEMI_BOLD, FONT * scale)?;
    unsafe {
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
        format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    }
    Ok(format)
}

/// La ventana no recibe el mouse ni el teclado: todo va a `DefWindowProcW`.
extern "system" fn passthrough_proc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
