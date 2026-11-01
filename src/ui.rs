//! Conserva eventos que llegan de forma reentrante durante llamadas a Windows/COM.
//! Se procesan cuando se libera el estado; nunca se invoca un callback mutable dos veces a la vez.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    PeekMessageW, MSG, PM_NOREMOVE, WM_MOUSEMOVE, WM_TIMER,
};

type Handler = extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

struct Event {
    handler: Handler,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
}

thread_local! {
    static EVENTS: RefCell<VecDeque<Event>> = const { RefCell::new(VecDeque::new()) };
    static FLUSHING: Cell<bool> = const { Cell::new(false) };
}

pub fn defer(handler: Handler, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) {
    EVENTS.with(|events| {
        let mut events = events.borrow_mut();
        // Agrupar movimientos y temporizadores consecutivos sin alterar el orden de clics
        // ni perder los punteros a paquetes de medios que debe liberar su receptor.
        if matches!(msg, WM_MOUSEMOVE | WM_TIMER) {
            if let Some(last) = events.back_mut() {
                if last.hwnd == hwnd && last.msg == msg && last.wparam == wparam {
                    last.lparam = lparam;
                    return;
                }
            }
        }
        events.push_back(Event {
            handler,
            hwnd,
            msg,
            wparam,
            lparam,
        });
    });
}

pub fn flush() {
    if FLUSHING.replace(true) {
        return;
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FLUSHING.set(false);
        }
    }
    let _reset = Reset;
    // Si otro estado sigue prestado, su evento vuelve al final y espera a que su dueño
    // termine. La pasada acotada evita girar en un bucle dentro de un callback.
    let count = EVENTS.with(|events| events.borrow().len());
    for _ in 0..count {
        let event = EVENTS.with(|events| events.borrow_mut().pop_front());
        if let Some(event) = event {
            (event.handler)(event.hwnd, event.msg, event.wparam, event.lparam);
        }
    }
}

/// WM_TIMER se genera con prioridad baja. Pedirlo sin retirarlo lo incorpora a la cola
/// aunque sigan llegando cuadros de VSync, para no aplazar indefinidamente la ocultación.
pub fn promote_timers() {
    let mut timer = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut timer, None, WM_TIMER, WM_TIMER, PM_NOREMOVE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONUP;

    thread_local! {
        static BUSY: Cell<bool> = const { Cell::new(false) };
        static RECEIVED: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
    }

    extern "system" fn handler(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if BUSY.get() {
            defer(handler, hwnd, msg, wparam, lparam);
        } else {
            RECEIVED.with(|events| events.borrow_mut().push(lparam.0));
        }
        LRESULT(0)
    }

    #[test]
    fn reentrada_conserva_clics_y_temporizadores_sin_girar_en_un_bucle() {
        BUSY.set(true);
        for (msg, payload) in [
            (WM_MOUSEMOVE, 1),
            (WM_MOUSEMOVE, 2),
            (WM_LBUTTONUP, 3),
            (WM_TIMER, 4),
        ] {
            handler(HWND::default(), msg, WPARAM(0), LPARAM(payload));
        }
        flush();
        assert!(RECEIVED.with(|events| events.borrow().is_empty()));
        BUSY.set(false);
        flush();
        assert_eq!(
            RECEIVED.with(|events| events.borrow().clone()),
            vec![2, 3, 4]
        );
        assert!(EVENTS.with(|events| events.borrow().is_empty()));
    }
}
