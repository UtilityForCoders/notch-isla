//! Acciones de compartir en la isla y diálogos de confirmación de Windows.

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::thread;

use windows::core::{w, Result, HSTRING, PWSTR};
use windows::Win32::Foundation::{ERROR_CANCELLED, HGLOBAL, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::Shell::{
    DragFinish, DragQueryFileW, FOLDERID_Downloads, FileOpenDialog, IFileOpenDialog,
    SHGetKnownFolderPath, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
    FOS_PATHMUSTEXIST, HDROP, KF_FLAG_DEFAULT, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, PostMessageW, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, WM_APP,
};

use crate::island::{Notice, NoticeIcon};
use crate::menu::{self, Item};
use crate::share::{self, Payload, Peer, ReceiveEvent};

pub const WM_SHARE_EVENT: u32 = WM_APP + 12;
pub const WM_SHARE_MENU: u32 = WM_APP + 13;
const TITLE: &str = "Compartir cerca";

enum Event {
    Devices(Payload, std::result::Result<Vec<Peer>, String>),
    Sent(std::result::Result<(), String>),
    Receive(ReceiveEvent),
    Finished(std::result::Result<Option<share::Received>, String>),
}

struct Ui {
    tx: Sender<Event>,
    rx: mpsc::Receiver<Event>,
    cancel: Option<Sender<()>>,
    receive_code: Option<String>,
    sending: bool,
    accepting: bool,
}

thread_local! {
    static UI: RefCell<Ui> = {
        let (tx, rx) = mpsc::channel();
        RefCell::new(Ui { tx, rx, cancel: None, receive_code: None, sending: false, accepting: false })
    };
}

fn reporter(hwnd: HWND) -> impl Fn(Event) + Send + 'static {
    let hwnd = hwnd.0 as isize;
    let tx = UI.with_borrow(|ui| ui.tx.clone());
    move |event| {
        if tx.send(event).is_ok() {
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as _)), WM_SHARE_EVENT, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn notify(detail: &str) {
    crate::with_app(|app| {
        app.show_notice_card(
            Notice {
                icon: NoticeIcon::Wireless,
                color: [83, 166, 255],
            },
            TITLE,
            detail,
            4500,
        );
    });
}

fn action(id: usize, label: impl Into<String>) -> Item {
    Item::Action {
        id,
        label: label.into(),
        checked: false,
    }
}

pub fn open(hwnd: HWND) {
    let (receiving, code, accepting) =
        UI.with_borrow(|ui| (ui.cancel.is_some(), ui.receive_code.clone(), ui.accepting));
    let mut items = vec![
        Item::Label(TITLE.into()),
        action(1, "Elegir fotos o archivos…"),
        action(2, "Enviar texto o imagen copiada"),
        Item::Separator,
    ];
    if accepting {
        items.push(Item::Label("Envío entrante en curso".into()));
    } else if receiving {
        if let Some(code) = code {
            items.push(Item::Label(format!("Código de recepción · {code}")));
        }
        items.push(action(3, "Detener recepción"));
    } else {
        items.push(action(3, "Activar recepción por 5 minutos"));
    }
    match menu::show(items) {
        Some(1) => match choose_files(hwnd) {
            Ok(Some(files)) => start_send(hwnd, Payload::Files(files)),
            Ok(None) => {}
            Err(_) => notify("No se pudo abrir el selector de archivos."),
        },
        Some(2) => match crate::share_preview::clipboard_image(hwnd)
            .map(Payload::Image)
            .or_else(|| clipboard_text(hwnd).map(Payload::Text))
        {
            Some(payload) => start_send(hwnd, payload),
            None => notify("Copia texto (hasta 256 KB) o una imagen (hasta 32 MB)."),
        },
        Some(3) if receiving => stop(),
        Some(3) => start_receive(hwnd),
        _ => {}
    }
}

pub fn drop_files(hwnd: HWND, drop: HDROP) {
    let files = unsafe {
        let count = DragQueryFileW(drop, u32::MAX, None);
        let files = (1..=share::MAX_ITEMS as u32).contains(&count).then(|| {
            (0..count)
                .filter_map(|index| {
                    let len = DragQueryFileW(drop, index, None);
                    let mut path = vec![0; len as usize + 1];
                    let written = DragQueryFileW(drop, index, Some(&mut path));
                    (written != 0)
                        .then(|| PathBuf::from(OsString::from_wide(&path[..written as usize])))
                })
                .collect::<Vec<_>>()
        });
        DragFinish(drop);
        files.filter(|files| files.len() == count as usize)
    };
    if let Some(files) = files {
        start_send(hwnd, Payload::Files(files));
    } else {
        notify("Arrastra entre 1 y 20 archivos.");
    }
}

fn choose_files(hwnd: HWND) -> Result<Option<Vec<PathBuf>>> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetTitle(w!("Elige fotos o archivos para compartir"))?;
        dialog.SetOptions(
            dialog.GetOptions()?
                | FOS_ALLOWMULTISELECT
                | FOS_FORCEFILESYSTEM
                | FOS_FILEMUSTEXIST
                | FOS_PATHMUSTEXIST,
        )?;
        if let Err(error) = dialog.Show(Some(hwnd)) {
            if error.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(None);
            }
            return Err(error);
        }
        let items = dialog.GetResults()?;
        let count = items.GetCount()?;
        if count as usize > share::MAX_ITEMS {
            notify("Selecciona un máximo de 20 archivos.");
            return Ok(None);
        }
        let paths = (0..count)
            .map(|index| {
                let path = items.GetItemAt(index)?.GetDisplayName(SIGDN_FILESYSPATH)?;
                Ok(owned_path(path))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(paths))
    }
}

unsafe fn owned_path(path: PWSTR) -> PathBuf {
    let value = PathBuf::from(OsString::from_wide(path.as_wide()));
    CoTaskMemFree(Some(path.0.cast()));
    value
}

fn clipboard_text(hwnd: HWND) -> Option<String> {
    // CF_UNICODETEXT, leído únicamente al elegir la acción.
    const UNICODE_TEXT: u32 = 13;
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let result = (|| {
            let memory = HGLOBAL(GetClipboardData(UNICODE_TEXT).ok()?.0);
            let size = GlobalSize(memory) / 2;
            let ptr = GlobalLock(memory).cast::<u16>();
            if ptr.is_null() {
                return None;
            }
            let text = std::slice::from_raw_parts(ptr, size.min(share::MAX_TEXT + 1));
            let result = text
                .iter()
                .position(|&c| c == 0)
                .map(|end| String::from_utf16_lossy(&text[..end]));
            let _ = GlobalUnlock(memory);
            result.filter(|text| !text.is_empty() && text.len() <= share::MAX_TEXT)
        })();
        let _ = CloseClipboard();
        result
    }
}

fn start_send(hwnd: HWND, payload: Payload) {
    let started = UI.with_borrow_mut(|ui| {
        let started = !ui.sending && ui.cancel.is_none() && !ui.accepting;
        if started {
            ui.sending = true;
        }
        started
    });
    if !started {
        notify("Termina primero el envío o la recepción actual.");
        return;
    }
    notify("Buscando equipos en tu red local…");
    let report = reporter(hwnd);
    thread::spawn(move || {
        let result = share::discover().map_err(|error| error.to_string());
        report(Event::Devices(payload, result));
    });
}

fn start_receive(hwnd: HWND) {
    if UI.with_borrow(|ui| ui.cancel.is_some() || ui.sending || ui.accepting) {
        notify("Termina primero el envío o la recepción actual.");
        return;
    }
    let downloads = unsafe {
        SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None)
            .map(|path| owned_path(path))
    };
    let Ok(downloads) = downloads else {
        notify("No se encontró la carpeta Descargas.");
        return;
    };
    let (stop, cancel) = mpsc::channel();
    UI.with_borrow_mut(|ui| ui.cancel = Some(stop));
    let report = reporter(hwnd);
    thread::spawn(move || {
        let result = share::receive(downloads, cancel, |event| report(Event::Receive(event)))
            .map_err(|error| error.to_string());
        report(Event::Finished(result));
    });
}

pub fn stop() {
    UI.with_borrow(|ui| {
        if let Some(cancel) = &ui.cancel {
            let _ = cancel.send(());
        }
    });
}

fn choose_device(hwnd: HWND, payload: Payload, peers: Vec<Peer>) -> bool {
    if peers.is_empty() {
        notify("Activa la recepción en el otro equipo y comprueba la red.");
        return false;
    }
    let description = match &payload {
        Payload::Image(_) => "Imagen copiada".into(),
        Payload::Text(text) => format!("Texto · {} caracteres", text.chars().count()),
        Payload::Files(files) if files.len() == 1 => files[0].file_name().map_or_else(
            || "1 archivo".into(),
            |name| name.to_string_lossy().into_owned(),
        ),
        Payload::Files(files) => format!("{} archivos", files.len()),
    };
    let mut items = vec![
        Item::Label(description),
        Item::Label("Elige un equipo y verifica su código".into()),
    ];
    items.extend(
        peers
            .iter()
            .enumerate()
            .map(|(i, peer)| action(i + 1, format!("{} · {}", peer.name, peer.code))),
    );
    let Some(peer) = menu::show(items).and_then(|id| peers.into_iter().nth(id - 1)) else {
        return false;
    };
    notify("Esperando que el destinatario acepte…");
    let report = reporter(hwnd);
    thread::spawn(move || {
        report(Event::Sent(
            share::send(peer, payload).map_err(|error| error.to_string()),
        ));
    });
    true
}

pub fn on_event(hwnd: HWND) {
    while let Ok(event) = UI.with_borrow(|ui| ui.rx.try_recv()) {
        match event {
            Event::Devices(payload, result) => {
                let sending = match result {
                    Ok(peers) => choose_device(hwnd, payload, peers),
                    Err(error) => {
                        notify(&format!("No se pudo buscar equipos. {error}"));
                        false
                    }
                };
                UI.with_borrow_mut(|ui| ui.sending = sending);
            }
            Event::Sent(result) => {
                UI.with_borrow_mut(|ui| ui.sending = false);
                match result {
                    Ok(()) => notify("El destinatario recibió el envío."),
                    Err(error) => notify(&format!("No se pudo enviar. {error}")),
                }
            }
            Event::Receive(ReceiveEvent::Ready(code)) => {
                notify(&format!("Recepción activa · Código {code}"));
                UI.with_borrow_mut(|ui| ui.receive_code = Some(code));
            }
            Event::Receive(ReceiveEvent::Offer {
                description,
                decision,
            }) => {
                let code = UI.with_borrow_mut(|ui| {
                    ui.accepting = true;
                    ui.receive_code.clone().unwrap_or_default()
                });
                let accepted = crate::share_preview::confirm(hwnd, &description, &code);
                let _ = decision.send(accepted);
                UI.with_borrow_mut(|ui| ui.accepting = false);
            }
            Event::Finished(result) => {
                UI.with_borrow_mut(|ui| {
                    ui.cancel = None;
                    ui.receive_code = None;
                });
                match result {
                    Ok(Some(share::Received::Text { sender, text })) => {
                        if let Err(error) = crate::share_preview::text(hwnd, &sender, &text) {
                            notify(&format!("No se pudo mostrar el texto. {error}"));
                        }
                    }
                    Ok(Some(share::Received::Image {
                        sender,
                        directory,
                        path,
                    })) => {
                        if let Err(error) = crate::share_preview::image(hwnd, &sender, &path) {
                            notify(&format!("No se pudo mostrar la imagen. {error}"));
                        }
                        drop(directory);
                    }
                    Ok(Some(share::Received::Files(path))) => {
                        let message = HSTRING::from(format!("Guardado en:\n{}", path.display()));
                        unsafe {
                            MessageBoxW(
                                None,
                                &message,
                                w!("Envío recibido · Notch Isla"),
                                MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
                            );
                        }
                    }
                    Ok(None) => notify("Recepción desactivada."),
                    Err(error) => notify(&format!("No se pudo recibir. {error}")),
                }
            }
        }
    }
}
