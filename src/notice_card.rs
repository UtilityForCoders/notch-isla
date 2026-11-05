//! Aviso flotante en su propia ventana, sin tomar el foco ni interceptar clics.

use std::time::{Duration, Instant};

use tiny_skia::Color;
use windows::core::{w, Result};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, SetWindowPos, ShowWindow, UpdateLayeredWindow, SWP_NOACTIVATE,
    SWP_NOSIZE, SWP_NOZORDER, SW_SHOWNOACTIVATE, ULW_ALPHA, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::{island, ActiveNotice, Surface, TextRenderer};

const WIDTH: f32 = 320.0;
const HEIGHT: f32 = 50.0;
const GAP: f32 = 8.0;
const FADE: Duration = Duration::from_millis(140);

pub struct NoticeCard {
    hwnd: HWND,
    surface: Surface,
    text: TextRenderer,
    notice: ActiveNotice,
    scale: f32,
    bounds: RECT,
    started: Instant,
    duration: Duration,
    text_due: Option<Instant>,
    pos: POINT,
    opacity: Option<u8>,
}

impl NoticeCard {
    #[cfg(test)]
    pub fn window(&self) -> HWND {
        self.hwnd
    }

    pub fn new(scale: f32, bounds: RECT, notice: ActiveNotice, ms: u32) -> Result<Self> {
        let surface = card_surface(scale)?;
        let text = TextRenderer::new(scale)?;
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT,
                w!("Static"),
                w!("Aviso de Notch Isla"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )?
        };
        let started = Instant::now();
        Ok(Self {
            hwnd,
            surface,
            text,
            notice,
            scale,
            bounds,
            started,
            duration: Duration::from_millis(ms.into()),
            text_due: Some(started),
            pos: POINT::default(),
            opacity: None,
        })
    }

    pub fn set_display(&mut self, scale: f32, bounds: RECT) -> Result<()> {
        if self.scale != scale {
            self.surface = card_surface(scale)?;
            self.text.set_scale(scale);
            self.scale = scale;
            self.text_due = Some(Instant::now());
        }
        self.bounds = bounds;
        Ok(())
    }

    pub fn place(&mut self, center_x: i32, bottom: i32) {
        let w = self.surface.width as i32;
        let h = self.surface.height as i32;
        let gap = (GAP * self.scale).round() as i32;
        let pos = POINT {
            x: (center_x - w / 2).clamp(
                self.bounds.left,
                (self.bounds.right - w).max(self.bounds.left),
            ),
            y: (bottom + gap).clamp(
                self.bounds.top,
                (self.bounds.bottom - h).max(self.bounds.top),
            ),
        };
        if self.pos != pos {
            if self.opacity.is_some() {
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        pos.x,
                        pos.y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            self.pos = pos;
        }
    }

    /// Actualiza el desvanecimiento o el texto largo. Devuelve la espera al próximo cuadro.
    pub fn advance(&mut self) -> Result<Option<Duration>> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.started);
        let remaining = self.duration.saturating_sub(elapsed);
        if remaining.is_zero() {
            return Ok(None);
        }
        let opacity = ((elapsed.min(remaining).as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
            * 255.0)
            .round() as u8;
        let repaint = self.text_due.is_some_and(|due| now >= due);
        if repaint {
            self.surface.pixmap.fill(Color::TRANSPARENT);
            let width = self.text.measure(&self.notice.title, &self.notice.subtitle);
            let areas = island::draw_notice_card(
                &mut self.surface.pixmap,
                self.scale,
                &self.notice.view,
                width,
            );
            self.surface.copy_to_dib();
            let (w, h) = (self.surface.width, self.surface.height);
            self.text_due = self
                .text
                .draw(
                    self.surface.dib(),
                    w,
                    h,
                    (0, 0, w, h),
                    &areas,
                    &self.notice.title,
                    &self.notice.subtitle,
                    elapsed.as_secs_f32(),
                )
                .map(|ms| now + Duration::from_millis(ms.into()));
        }
        if repaint || self.opacity != Some(opacity) {
            let size = SIZE {
                cx: self.surface.width as i32,
                cy: self.surface.height as i32,
            };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: opacity,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            unsafe {
                UpdateLayeredWindow(
                    self.hwnd,
                    None,
                    Some(&self.pos),
                    Some(&size),
                    Some(self.surface.dc),
                    Some(&POINT::default()),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                )?;
                if self.opacity.is_none() {
                    let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                }
            }
            self.opacity = Some(opacity);
        }
        let next_frame = if elapsed < FADE || remaining <= FADE {
            Duration::from_millis(16).min(remaining)
        } else {
            remaining - FADE
        };
        Ok(Some(self.text_due.map_or(next_frame, |due| {
            next_frame.min(due.saturating_duration_since(now))
        })))
    }
}

impl Drop for NoticeCard {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn card_surface(scale: f32) -> Result<Surface> {
    Surface::new(
        (WIDTH * scale).round() as u32,
        (HEIGHT * scale).round() as u32,
    )
}
