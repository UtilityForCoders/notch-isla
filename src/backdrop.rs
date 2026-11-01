//! Fondo desenfocado compuesto por Windows detrás de la ventana layered del contenido.
//! No captura la pantalla ni vuelve a pintar el texto: el compositor actualiza el fondo.

use std::cell::{OnceCell, RefCell};
use windows::core::{implement, Interface, Result, BOOL, GUID, HSTRING, PCWSTR};
use windows::Foundation::{IPropertyValue, PropertyValue};
use windows::Graphics::Effects::{
    IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectSource_Impl, IGraphicsEffect_Impl,
};
use windows::System::DispatcherQueueController;
use windows::Win32::Foundation::{E_INVALIDARG, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::CLSID_D2D1GaussianBlur;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::Graphics::Direct2D::{
    IGraphicsEffectD2D1Interop, IGraphicsEffectD2D1Interop_Impl, GRAPHICS_EFFECT_PROPERTY_MAPPING,
};
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SetWindowPos, ShowWindow,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WNDCLASSW, WS_DISABLED, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionEffectSourceParameter, CompositionRoundedRectangleGeometry, Compositor, SpriteVisual,
};
use windows_numerics::{Vector2, Vector3};

thread_local! {
    static COMPOSITOR: OnceCell<Option<(DispatcherQueueController, Compositor)>> = const { OnceCell::new() };
}

fn compositor() -> Result<Compositor> {
    COMPOSITOR.with(|cell| {
        cell.get_or_init(|| {
            let controller = unsafe {
                CreateDispatcherQueueController(DispatcherQueueOptions {
                    dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
                    threadType: DQTYPE_THREAD_CURRENT,
                    apartmentType: DQTAT_COM_STA,
                })
            }
            .ok()?;
            Some((controller, Compositor::new().ok()?))
        })
        .as_ref()
        .map(|(_, compositor)| compositor.clone())
        .ok_or_else(|| E_INVALIDARG.into())
    })
}

fn enable_host_backdrop(hwnd: HWND) -> Result<()> {
    let enabled = BOOL(1);
    unsafe {
        let result = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_HOSTBACKDROPBRUSH,
            (&enabled as *const BOOL).cast(),
            std::mem::size_of::<BOOL>() as u32,
        );
        if result.is_ok() {
            return result;
        }
        // Windows 10 no expone DWMWA_USE_HOSTBACKDROPBRUSH. Su política de composición
        // equivalente se resuelve dinámicamente para tolerar que no esté disponible.
        #[repr(C)]
        struct AccentPolicy {
            state: u32,
            flags: u32,
            color: u32,
            animation: u32,
        }
        #[repr(C)]
        struct AttributeData {
            attribute: u32,
            data: *mut std::ffi::c_void,
            size: usize,
        }
        let module = GetModuleHandleW(windows::core::w!("user32.dll"))?;
        let Some(proc) = GetProcAddress(module, windows::core::s!("SetWindowCompositionAttribute"))
        else {
            return result;
        };
        let apply: unsafe extern "system" fn(HWND, *mut AttributeData) -> BOOL =
            std::mem::transmute(proc);
        let mut policy = AccentPolicy {
            state: 5,
            flags: 0,
            color: 0,
            animation: 0,
        };
        let mut data = AttributeData {
            attribute: 19,
            data: (&mut policy as *mut AccentPolicy).cast(),
            size: std::mem::size_of::<AccentPolicy>(),
        };
        if apply(hwnd, &mut data).as_bool() {
            Ok(())
        } else {
            result
        }
    }
}

#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct GaussianBlur {
    source: IGraphicsEffectSource,
    radius: f32,
    name: RefCell<HSTRING>,
}

impl IGraphicsEffectSource_Impl for GaussianBlur_Impl {}

impl IGraphicsEffect_Impl for GaussianBlur_Impl {
    fn Name(&self) -> Result<HSTRING> {
        Ok(self.name.borrow().clone())
    }
    fn SetName(&self, name: &HSTRING) -> Result<()> {
        *self.name.borrow_mut() = name.clone();
        Ok(())
    }
}

impl IGraphicsEffectD2D1Interop_Impl for GaussianBlur_Impl {
    fn GetEffectId(&self) -> Result<GUID> {
        Ok(CLSID_D2D1GaussianBlur)
    }
    fn GetNamedPropertyMapping(
        &self,
        _: &PCWSTR,
        _: *mut u32,
        _: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING,
    ) -> Result<()> {
        Err(E_INVALIDARG.into())
    }
    fn GetPropertyCount(&self) -> Result<u32> {
        Ok(3)
    }
    fn GetProperty(&self, index: u32) -> Result<IPropertyValue> {
        match index {
            0 => PropertyValue::CreateSingle(self.radius)?.cast(),
            // Optimización equilibrada y borde duro para no oscurecer el contorno.
            1 | 2 => PropertyValue::CreateUInt32(1)?.cast(),
            _ => Err(E_INVALIDARG.into()),
        }
    }
    fn GetSource(&self, index: u32) -> Result<IGraphicsEffectSource> {
        if index == 0 {
            Ok(self.source.clone())
        } else {
            Err(E_INVALIDARG.into())
        }
    }
    fn GetSourceCount(&self) -> Result<u32> {
        Ok(1)
    }
}

pub struct Backdrop {
    hwnd: HWND,
    compositor: Compositor,
    target: DesktopWindowTarget,
    visual: SpriteVisual,
    geometry: CompositionRoundedRectangleGeometry,
    radius: u32,
    shown: bool,
    placement: Option<(i32, i32, u32, u32)>,
}

impl Backdrop {
    pub fn new() -> Result<Self> {
        let compositor = compositor()?;
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = windows::core::w!("NotchIslaDesenfoque");
            RegisterClassW(&WNDCLASSW {
                hInstance: instance.into(),
                lpszClassName: class,
                lpfnWndProc: Some(wndproc),
                ..Default::default()
            });
            let hwnd = CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST,
                class,
                windows::core::w!("Fondo de Notch Isla"),
                WS_POPUP | WS_DISABLED,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            let setup = (|| {
                enable_host_backdrop(hwnd)?;
                let interop: ICompositorDesktopInterop = compositor.cast()?;
                let target = interop.CreateDesktopWindowTarget(hwnd, false)?;
                let visual = compositor.CreateSpriteVisual()?;
                let geometry = compositor.CreateRoundedRectangleGeometry()?;
                visual.SetClip(&compositor.CreateGeometricClipWithGeometry(&geometry)?)?;
                target.SetRoot(&visual)?;
                Ok((target, visual, geometry))
            })();
            match setup {
                Ok((target, visual, geometry)) => Ok(Self {
                    hwnd,
                    compositor,
                    target,
                    visual,
                    geometry,
                    radius: u32::MAX,
                    shown: false,
                    placement: None,
                }),
                Err(error) => {
                    let _ = DestroyWindow(hwnd);
                    Err(error)
                }
            }
        }
    }

    /// Rectángulo y radio de esquina en píxeles físicos. La capa queda justo bajo el contenido.
    pub fn show(
        &mut self,
        content: HWND,
        viewport: (i32, i32, u32, u32),
        shape: (f32, f32, f32, f32, f32),
        radius: u32,
        opacity: f32,
    ) -> bool {
        let result = self.update(content, viewport, shape, radius, opacity);
        if result.is_err() {
            self.hide();
        }
        result.is_ok()
    }

    fn update(
        &mut self,
        content: HWND,
        viewport: (i32, i32, u32, u32),
        (x, y, w, h, corner): (f32, f32, f32, f32, f32),
        radius: u32,
        opacity: f32,
    ) -> Result<()> {
        if self.radius != radius {
            let name = HSTRING::from("Backdrop");
            let source = CompositionEffectSourceParameter::Create(&name)?;
            let effect: IGraphicsEffect = GaussianBlur {
                source: source.cast()?,
                radius: radius as f32,
                name: RefCell::new(HSTRING::new()),
            }
            .into();
            let brush = self
                .compositor
                .CreateEffectFactory(&effect)?
                .CreateBrush()?;
            brush.SetSourceParameter(&name, &self.compositor.CreateBackdropBrush()?)?;
            self.visual.SetBrush(&brush)?;
            self.radius = radius;
        }
        let size = Vector2 {
            X: w.max(1.0),
            Y: h.max(1.0),
        };
        self.visual.SetSize(size)?;
        self.visual.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
        self.geometry.SetSize(size)?;
        self.geometry.SetCornerRadius(Vector2 {
            X: corner,
            Y: corner,
        })?;
        self.visual.SetOpacity(opacity.clamp(0.0, 1.0))?;
        // La ventana conserva el tamaño del lienzo. Solo cambia el visual dentro de una
        // transacción de composición: no se redimensiona el HWND a mitad de la animación.
        if !self.shown || self.placement != Some(viewport) {
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    Some(content),
                    viewport.0,
                    viewport.1,
                    viewport.2 as i32,
                    viewport.3 as i32,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                )?;
            }
            self.placement = Some(viewport);
        }
        self.shown = true;
        Ok(())
    }

    pub fn hide(&mut self) {
        if self.shown {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.shown = false;
        }
    }
}

impl Drop for Backdrop {
    fn drop(&mut self) {
        let _ = self
            .target
            .SetRoot(None::<&windows::UI::Composition::Visual>);
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;

    #[test]
    #[ignore = "requiere escritorio interactivo de Windows y compositor DWM"]
    fn compone_cambia_radio_y_oculta_sin_animacion() {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
        }
        let mut backdrop = Backdrop::new().expect("host backdrop disponible");
        // Fuera de la pantalla: verifica las APIs reales sin tapar el escritorio.
        for radius in [4, 10, 16, 32] {
            backdrop
                .update(
                    windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST,
                    (-32000, -32000, 200, 40),
                    (0.0, 0.0, 200.0, 40.0, 20.0),
                    radius,
                    1.0,
                )
                .expect("efecto gaussiano y ventana compuesta");
            assert!(unsafe { IsWindowVisible(backdrop.hwnd).as_bool() });
        }
        backdrop.hide();
        assert!(!unsafe { IsWindowVisible(backdrop.hwnd).as_bool() });
        assert!(!backdrop.shown);
        backdrop.hide();
    }

    #[test]
    #[ignore = "muestra un patrón de prueba brevemente para medir el desenfoque real en pantalla"]
    fn difumina_pixeles_del_escritorio() {
        use std::time::{Duration, Instant};
        use windows::Win32::Foundation::RECT;
        use windows::Win32::Graphics::Gdi::{
            CreateSolidBrush, DeleteObject, FillRect, GetDC, GetPixel, ReleaseDC,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, HWND_TOPMOST, MSG, PM_REMOVE, SW_SHOWNOACTIVATE,
        };
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
            let mut backdrop = Backdrop::new().unwrap();
            let instance = GetModuleHandleW(None).unwrap();
            let source = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                windows::core::w!("NotchIslaDesenfoque"),
                windows::core::w!("Patrón de prueba"),
                WS_POPUP,
                200,
                200,
                400,
                200,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .unwrap();
            let _ = ShowWindow(source, SW_SHOWNOACTIVATE);
            let dc = GetDC(Some(source));
            for x in 0..40 {
                let brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(if x % 2 == 0 {
                    0
                } else {
                    0xffffff
                }));
                FillRect(
                    dc,
                    &RECT {
                        left: x * 10,
                        top: 0,
                        right: x * 10 + 10,
                        bottom: 200,
                    },
                    brush,
                );
                let _ = DeleteObject(brush.into());
            }
            ReleaseDC(Some(source), dc);
            let pump = || {
                let until = Instant::now() + Duration::from_millis(300);
                while Instant::now() < until {
                    let mut msg = MSG::default();
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        DispatchMessageW(&msg);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            };
            let sample = |x| {
                let screen = GetDC(None);
                let a = GetPixel(screen, x, 300).0 & 255;
                let b = GetPixel(screen, x + 10, 300).0 & 255;
                ReleaseDC(None, screen);
                (a, b)
            };
            pump();
            let baseline = sample(385);
            assert!(
                baseline.0.abs_diff(baseline.1) > 220,
                "patrón visible: {baseline:?}"
            );
            backdrop
                .update(
                    HWND_TOPMOST,
                    (200, 200, 400, 200),
                    (0.0, 0.0, 400.0, 200.0, 20.0),
                    0,
                    1.0,
                )
                .unwrap();
            pump();
            let unblurred = sample(385);
            assert!(
                unblurred.0.abs_diff(unblurred.1) > 180,
                "el radio cero no debe difuminar: {unblurred:?}"
            );
            for expanded in [true, false, true, false] {
                let shape = if expanded {
                    (0.0, 0.0, 400.0, 200.0, 20.0)
                } else {
                    (140.0, 60.0, 120.0, 80.0, 20.0)
                };
                backdrop
                    .update(HWND_TOPMOST, (200, 200, 400, 200), shape, 24, 1.0)
                    .unwrap();
                pump();
                let (a, b) = sample(385);
                assert!(
                    a.abs_diff(b) < 60 && a > 40 && b > 40,
                    "contraste tras difuminar: {a}, {b}"
                );
                if !expanded {
                    let outside = sample(305);
                    assert!(
                        outside.0.abs_diff(outside.1) > 220,
                        "no deben quedar restos al encogerse: {outside:?}"
                    );
                }
            }
            backdrop.hide();
            pump();
            let after = sample(385);
            let _ = DestroyWindow(source);
            assert!(
                after.0.abs_diff(after.1) > 220,
                "el fondo desaparece al ocultarse: {after:?}"
            );
        }
    }
}
