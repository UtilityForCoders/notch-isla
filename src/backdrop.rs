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
use windows_numerics::Vector2;

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
        rect: (f32, f32, f32, f32),
        corner: f32,
        radius: u32,
        opacity: f32,
    ) -> bool {
        let result = self.update(content, rect, corner, radius, opacity);
        if result.is_err() {
            self.hide();
        }
        result.is_ok()
    }

    fn update(
        &mut self,
        content: HWND,
        (x, y, w, h): (f32, f32, f32, f32),
        corner: f32,
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
            brush.SetSourceParameter(&name, &self.compositor.CreateHostBackdropBrush()?)?;
            self.visual.SetBrush(&brush)?;
            self.radius = radius;
        }
        let size = Vector2 {
            X: w.max(1.0),
            Y: h.max(1.0),
        };
        self.visual.SetSize(size)?;
        self.geometry.SetSize(size)?;
        self.geometry.SetCornerRadius(Vector2 {
            X: corner,
            Y: corner,
        })?;
        self.visual.SetOpacity(opacity.clamp(0.0, 1.0))?;
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(content),
                x.round() as i32,
                y.round() as i32,
                w.ceil().max(1.0) as i32,
                h.ceil().max(1.0) as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
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
                    (-32000.0, -32000.0, 200.0, 40.0),
                    20.0,
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
}
